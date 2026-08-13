//! Core domain primitives shared by every Kiln crate.
//!
//! `kiln-core` deliberately depends on no other Kiln crate. It owns the types
//! that every layer needs to agree on:
//!
//! - [`Error`] — the single structured error type used across the workspace.
//! - [`Version`] / [`VersionReq`] — how Kiln talks about runtime versions.
//! - [`Platform`] — operating system, CPU architecture and libc flavour.
//! - [`Digest`] — content addresses for cached artifacts.
//! - [`paths::KilnPaths`] — where Kiln keeps its store on disk.
//! - [`ui::Ui`] — the terminal output layer.

#![forbid(unsafe_code)]

pub mod artifact;
pub mod digest;
pub mod error;
pub mod paths;
pub mod platform;
pub mod source;
pub mod ui;
pub mod version;

pub use artifact::{ArtifactFormat, RuntimeLayout};
pub use digest::{Digest, HashAlgorithm, Hasher};
pub use error::{Error, ErrorKind, Hint, Result};
pub use paths::KilnPaths;
pub use platform::{Arch, Libc, Os, Platform};
pub use source::SourceLocation;
pub use ui::{ColorChoice, Ui};
pub use version::{PartialVersion, Prerelease, Version, VersionAlias, VersionReq};

/// The version of the Kiln release this build belongs to.
pub const KILN_VERSION: &str = env!("CARGO_PKG_VERSION");
