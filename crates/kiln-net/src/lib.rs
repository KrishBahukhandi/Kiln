//! The only crate in Kiln allowed to talk to the network.
//!
//! Confining HTTP to one crate is what makes "does this command need the
//! network?" a question you can answer by reading the dependency graph. Nothing
//! else in the workspace can accidentally acquire the ability to make a request.
//!
//! Three things live here:
//!
//! - [`Http`] — a blocking client, with Kiln's error vocabulary rather than the
//!   transport's.
//! - [`Download`] — fetching an artifact while hashing it in flight, so the
//!   bytes are read exactly once on their way to disk.
//! - [`MetadataCache`] — a small time-to-live cache for release indexes, so a
//!   second `kiln install` in a different project does not re-fetch 250 KB of
//!   JSON that has not changed.
//!
//! # Blocking, on purpose
//!
//! Kiln uses a blocking client and no async runtime. `RuntimeProvider` has to
//! stay object-safe for the registry and for a future plugin boundary, which
//! async-fn-in-trait does not allow without boxing every call. Downloads are
//! parallelised with threads instead, which is the right shape for a handful of
//! large transfers.

#![forbid(unsafe_code)]

pub mod cache;
pub mod client;
pub mod download;

pub use cache::MetadataCache;
pub use client::Http;
pub use download::{Download, Progress, SilentProgress, verify_file};
