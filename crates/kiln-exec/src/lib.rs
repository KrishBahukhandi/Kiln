//! The project environment, and running things inside it.
//!
//! Kiln never modifies a shell profile, a system directory, or anything outside
//! its own store. An environment is assembled in memory, handed to one child
//! process, and forgotten when that process exits. Leaving the shell leaves no
//! trace, which is the only honest way to offer `kiln shell`.
//!
//! # What lives here
//!
//! [`Environment`] composes `PATH` and the project's variables, with the
//! precedence and de-duplication rules that make a Kiln runtime win over a
//! system one. [`process`] runs a program inside it, resolving the executable
//! against that composed `PATH` rather than trusting the platform to do it.

#![forbid(unsafe_code)]

pub mod env;
pub mod process;

pub use env::Environment;
pub use process::{Exit, resolve_program, run, spawn_and_wait};
