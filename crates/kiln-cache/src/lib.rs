//! Kiln's content-addressed artifact store.
//!
//! Artifacts are stored under the digest of their contents, never under a
//! human-readable name:
//!
//! ```text
//! ~/.kiln/store/sha256/9f/9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08/
//! ```
//!
//! Two consequences follow, and they are the reason the store is built this way:
//!
//! - **Sharing is automatic.** Two projects that need byte-identical copies of
//!   Node.js 22.14.0 converge on one directory without either of them knowing
//!   the other exists. Nothing is duplicated and nothing has to be reconciled.
//! - **Corruption is detectable.** An entry's name is a claim about its contents
//!   that can be checked at any time against nothing but the bytes on disk.
//!
//! Entries are immutable once written. Installing an artifact means unpacking it
//! into `staging/` and renaming the result into `store/` only after its digest
//! has been verified, so a partially written artifact is never reachable under a
//! name that says it is complete.
//!
//! # Status
//!
//! This release implements the layout, lookup, enumeration, extraction and
//! atomic insertion. Verification and garbage collection arrive in Phase 3:
//! both need a canonical way to hash a directory tree, and that decision fixes
//! the meaning of "this entry is intact" permanently. See `docs/architecture.md`.

#![forbid(unsafe_code)]

pub mod archive;
pub mod store;

pub use archive::extract;
pub use store::{ContentStore, EntryMeta, StoreEntry, unix_now};
