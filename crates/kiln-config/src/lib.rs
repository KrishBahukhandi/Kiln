//! The `kiln.toml` contract.
//!
//! This crate owns everything about the manifest: its schema, how it is parsed,
//! what makes it valid, how a project containing one is found, and how a new one
//! is written.
//!
//! It knows nothing about *which* runtimes exist. `node = "22"` and
//! `frobnicator = "22"` are equally well-formed here; deciding whether a name
//! corresponds to a real provider belongs to [`kiln-resolver`], which is what
//! lets a runtime be added without touching the configuration layer.
//!
//! ```
//! use std::path::Path;
//! use kiln_config::parse_str;
//!
//! let manifest = parse_str(
//!     r#"
//!     [project]
//!     name = "example-app"
//!
//!     [runtime]
//!     node = "22.14.0"
//!     "#,
//!     Path::new("kiln.toml"),
//! )?;
//!
//! assert_eq!(manifest.project.name.as_str(), "example-app");
//! assert_eq!(manifest.requirement_count(), 1);
//! # Ok::<(), kiln_core::Error>(())
//! ```
//!
//! [`kiln-resolver`]: https://docs.rs/kiln-resolver

#![forbid(unsafe_code)]

pub mod command;
pub mod discover;
pub mod manifest;
pub mod names;
pub mod parse;
pub mod render;

pub use command::{CommandSpec, split_words};
pub use discover::{Project, find_manifest};
pub use manifest::{LOCKFILE_FILE, MANIFEST_FILE, Manifest, Project as ProjectSection};
pub use names::{CommandName, EnvVarName, ProjectName, RuntimeName, ServiceName};
pub use parse::{parse_file, parse_str, write_atomically};
pub use render::ManifestDraft;
