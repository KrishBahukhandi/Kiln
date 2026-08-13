//! Validated identifiers used as keys in `kiln.toml`.
//!
//! Each of these is a newtype whose `FromStr` enforces the rules. That placement
//! is deliberate: because they deserialise through `FromStr`, the TOML parser
//! attaches the byte span of the offending value to the failure, and Kiln can
//! point a caret at the exact token instead of saying "somewhere in your config".

use std::fmt;
use std::str::FromStr;

use kiln_core::error::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Environment variables Kiln refuses to let a repository set.
///
/// `PATH` is Kiln's to compose — a project that overwrites it would silently
/// disable the environment it just asked for. The loader-injection variables
/// change which code *every* child process executes, which both defeats
/// reproducibility and turns cloning a repository into running its code.
const RESERVED_ENVIRONMENT: &[(&str, &str)] = &[
    (
        "PATH",
        "Kiln composes PATH from the runtimes this project pins",
    ),
    (
        "LD_PRELOAD",
        "it injects a library into every process started in this environment",
    ),
    (
        "DYLD_INSERT_LIBRARIES",
        "it injects a library into every process started in this environment",
    ),
];

/// The project's name, from `[project] name`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectName(String);

/// The name of a runtime or tool, e.g. `node`, `python`, `pnpm`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeName(String);

/// The name of a service, e.g. `postgres`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceName(String);

/// The name of a project command, e.g. `dev` or `test:unit`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandName(String);

/// The name of an environment variable.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnvVarName(String);

impl ProjectName {
    /// Validate and wrap a project name.
    ///
    /// The character set is restricted because the name reaches the filesystem
    /// and the terminal: `..`, separators and control characters must never
    /// survive to become part of a path or an escape sequence.
    pub fn parse(raw: &str) -> Result<Self> {
        let name = raw.trim();
        if name.is_empty() {
            return Err(invalid("project name", raw, "it is empty"));
        }
        if name.len() > 64 {
            return Err(invalid(
                "project name",
                raw,
                "it is longer than 64 characters",
            ));
        }
        if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(invalid(
                "project name",
                raw,
                "it must start with a letter or a digit",
            )
            .expected("letters, digits, and then any of `-` `_` `.`"));
        }
        if let Some(bad) = name
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        {
            return Err(invalid(
                "project name",
                raw,
                format!("`{bad}` is not allowed in a project name"),
            )
            .expected("letters, digits, and then any of `-` `_` `.`"));
        }
        if name.contains("..") {
            return Err(invalid(
                "project name",
                raw,
                "`..` is not allowed, because the name is used to build paths",
            ));
        }
        Ok(ProjectName(name.to_string()))
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Shared rule for runtime and service identifiers.
///
/// Lowercase only: `Node` and `node` must not be able to name two different
/// entries in the same map, or the resolved environment stops being a function
/// of the manifest.
fn parse_lowercase_ident(what: &str, raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(invalid(what, raw, "it is empty"));
    }
    if name.len() > 32 {
        return Err(invalid(what, raw, "it is longer than 32 characters"));
    }
    if name.chars().any(|c| c.is_ascii_uppercase()) {
        return Err(invalid(what, raw, "it contains uppercase letters")
            .hint(format!("write it in lowercase: `{}`", name.to_lowercase())));
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) {
        return Err(invalid(what, raw, "it must start with a letter or a digit"));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')))
    {
        return Err(invalid(what, raw, format!("`{bad}` is not allowed here"))
            .expected("lowercase letters, digits, and then any of `-` `_` `.`"));
    }
    if name.contains("..") {
        return Err(invalid(
            what,
            raw,
            "`..` is not allowed, because names build paths",
        ));
    }
    Ok(name.to_string())
}

impl RuntimeName {
    /// Validate and wrap a runtime or tool name.
    pub fn parse(raw: &str) -> Result<Self> {
        parse_lowercase_ident("runtime name", raw).map(RuntimeName)
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ServiceName {
    /// Validate and wrap a service name.
    pub fn parse(raw: &str) -> Result<Self> {
        parse_lowercase_ident("service name", raw).map(ServiceName)
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl CommandName {
    /// Validate and wrap a command name. `:` is permitted so that the
    /// `test:unit` convention carries over from npm scripts.
    pub fn parse(raw: &str) -> Result<Self> {
        let name = raw.trim();
        if name.is_empty() {
            return Err(invalid("command name", raw, "it is empty"));
        }
        if name.len() > 64 {
            return Err(invalid(
                "command name",
                raw,
                "it is longer than 64 characters",
            ));
        }
        if !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return Err(invalid(
                "command name",
                raw,
                "it must start with a letter or a digit",
            ));
        }
        if let Some(bad) = name
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')))
        {
            return Err(invalid(
                "command name",
                raw,
                format!("`{bad}` is not allowed in a command name"),
            )
            .expected("letters, digits, and then any of `-` `_` `.` `:`"));
        }
        Ok(CommandName(name.to_string()))
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl EnvVarName {
    /// Validate and wrap an environment variable name.
    pub fn parse(raw: &str) -> Result<Self> {
        let name = raw.trim();
        if name.is_empty() {
            return Err(invalid("environment variable name", raw, "it is empty"));
        }
        if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            return Err(invalid(
                "environment variable name",
                raw,
                "it must start with a letter or an underscore",
            ));
        }
        if let Some(bad) = name
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
        {
            return Err(invalid(
                "environment variable name",
                raw,
                format!("`{bad}` is not allowed in an environment variable name"),
            )
            .expected("letters, digits and underscores"));
        }
        if let Some((_, why)) = RESERVED_ENVIRONMENT
            .iter()
            .find(|(reserved, _)| *reserved == name)
        {
            return Err(Error::config(format!("`{name}` cannot be set from kiln.toml"))
                .because(*why)
                .hint("set it in your shell, outside the project environment, if you really need it"));
        }
        Ok(EnvVarName(name.to_string()))
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn invalid(what: &str, raw: &str, reason: impl Into<String>) -> Error {
    Error::config(format!("invalid {what} `{}`", raw.trim())).because(reason)
}

/// Generate the boilerplate every identifier newtype needs.
macro_rules! impl_name_traits {
    ($ty:ty) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl FromStr for $ty {
            type Err = Error;
            fn from_str(s: &str) -> Result<Self> {
                Self::parse(s)
            }
        }

        impl Serialize for $ty {
            fn serialize<S: Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::parse(&raw)
                    .map_err(|e| serde::de::Error::custom(crate::parse::serde_message(&e)))
            }
        }
    };
}

impl_name_traits!(ProjectName);
impl_name_traits!(RuntimeName);
impl_name_traits!(ServiceName);
impl_name_traits!(CommandName);
impl_name_traits!(EnvVarName);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_project_names() {
        for name in ["app", "my-app", "my_app", "app.v2", "a", "App2"] {
            assert!(ProjectName::parse(name).is_ok(), "`{name}` should be valid");
        }
    }

    #[test]
    fn rejects_project_names_that_could_escape_a_directory() {
        for name in [
            "", "..", "../etc", "a/b", "a\\b", ".hidden", "-leading", "a..b",
        ] {
            assert!(
                ProjectName::parse(name).is_err(),
                "`{name}` should be rejected"
            );
        }
    }

    #[test]
    fn rejects_overlong_project_names() {
        assert!(ProjectName::parse(&"a".repeat(64)).is_ok());
        assert!(ProjectName::parse(&"a".repeat(65)).is_err());
    }

    #[test]
    fn runtime_names_are_lowercase() {
        assert!(RuntimeName::parse("node").is_ok());
        assert!(RuntimeName::parse("python3").is_ok());
        assert!(RuntimeName::parse("dotnet-sdk").is_ok());

        let err = RuntimeName::parse("Node").unwrap_err();
        assert!(err.hints().iter().any(|h| h.text().contains("node")));
    }

    #[test]
    fn rejects_runtime_names_with_separators() {
        for name in ["", "no de", "node/js", "../node", "a..b"] {
            assert!(
                RuntimeName::parse(name).is_err(),
                "`{name}` should be rejected"
            );
        }
    }

    #[test]
    fn command_names_allow_the_npm_colon_convention() {
        assert!(CommandName::parse("dev").is_ok());
        assert!(CommandName::parse("test:unit").is_ok());
        assert!(CommandName::parse("build.prod").is_ok());
        assert!(CommandName::parse("has space").is_err());
    }

    #[test]
    fn environment_names_follow_posix() {
        for name in ["NODE_ENV", "_private", "A1"] {
            assert!(EnvVarName::parse(name).is_ok(), "`{name}` should be valid");
        }
        for name in ["", "1ABC", "NODE-ENV", "NODE ENV", "NODE=ENV"] {
            assert!(
                EnvVarName::parse(name).is_err(),
                "`{name}` should be rejected"
            );
        }
    }

    #[test]
    fn path_cannot_be_overridden_by_a_project() {
        let err = EnvVarName::parse("PATH").unwrap_err();
        assert!(err.summary().contains("PATH"));
        assert!(err.reason().unwrap().contains("composes PATH"));
    }

    #[test]
    fn loader_injection_variables_are_refused() {
        for name in ["LD_PRELOAD", "DYLD_INSERT_LIBRARIES"] {
            let err = EnvVarName::parse(name).unwrap_err();
            assert!(
                err.reason().unwrap().contains("injects a library"),
                "{name}"
            );
        }
        // Library search paths are configuration, not injection, so they stay legal.
        assert!(EnvVarName::parse("LD_LIBRARY_PATH").is_ok());
    }

    #[test]
    fn names_round_trip_through_display() {
        assert_eq!(ProjectName::parse("my-app").unwrap().to_string(), "my-app");
        assert_eq!(RuntimeName::parse("node").unwrap().as_str(), "node");
        assert_eq!(
            EnvVarName::parse(" NODE_ENV ").unwrap().as_str(),
            "NODE_ENV"
        );
    }
}
