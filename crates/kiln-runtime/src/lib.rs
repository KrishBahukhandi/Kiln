//! What Kiln knows about individual runtimes.
//!
//! A [`RuntimeProvider`] is the single place any runtime-specific knowledge is
//! allowed to live. Nothing outside this crate may branch on whether it is
//! looking at Node.js or Python: adding Go should mean adding one file here and
//! one line in [`Registry::builtin`], and touching nothing else.
//!
//! # What a provider does, and does not do
//!
//! A provider answers three questions and declares one fact:
//!
//! - *What versions exist?* ([`RuntimeProvider::releases`])
//! - *Where is this one, and what must it hash to?* ([`RuntimeProvider::artifact`])
//! - *Does this project already use me, and at what version?* ([`RuntimeProvider::detect`])
//! - *How is my archive laid out?* ([`RuntimeProvider::layout`])
//!
//! It does **not** download, verify or extract. Those are shared, so the
//! security-critical code exists once rather than once per runtime, and adding
//! Go means writing a file rather than a project.
//!
//! [`RuntimeProvider::resolve`] is provided rather than implemented per runtime,
//! and short-circuits an exact requirement without fetching a release index at
//! all — so a fully pinned project depends on less upstream infrastructure
//! staying up.

#![forbid(unsafe_code)]

pub mod checksums;
pub mod detect;
pub mod provider;
pub mod providers;
pub mod registry;

pub use provider::{
    ArtifactSpec, Evidence, ProviderContext, Release, RuntimeKind, RuntimeProvider, ToolEvidence,
};
pub use registry::Registry;
