//! From "what this project wants" to "what this machine will run".
//!
//! Two files describe a Kiln environment, and the difference between them is the
//! whole design:
//!
//! ```text
//! kiln.toml   what a human asked for      node = "22"
//!     ↓
//! kiln.lock   what that resolved to       node 22.14.0, sha256:…, for macos-aarch64
//! ```
//!
//! A manifest may be deliberately loose. A lockfile never is. Resolution is the
//! step that turns one into the other, and it is the only step allowed to depend
//! on the outside world.
//!
//! # The pipeline
//!
//! - [`plan()`] — checking a manifest against the available providers and the host
//!   platform. Entirely offline, and enough to catch a misspelled runtime or an
//!   unsupported machine before anything is downloaded.
//! - [`lockfile`] — the on-disk format, complete with forward-compatibility
//!   handling, so a lockfile written today stays readable.
//!
//! - [`resolve()`] — choosing an exact version and artifact, reusing `kiln.lock`
//!   where it still applies so a pinned project resolves without the network.
//! - [`install()`] — downloading, verifying and storing what was resolved.

#![forbid(unsafe_code)]

pub mod activate;
pub mod drift;
pub mod install;
pub mod lockfile;
pub mod plan;
pub mod resolve;

pub use activate::{ActiveRuntime, ProjectEnvironment, activate};
pub use drift::{Drift, DriftReason, drift, is_up_to_date, locked_error};
pub use install::{
    DEFAULT_JOBS, InstallOutcome, InstalledRuntime, MAX_JOBS, Observer, SilentObserver,
    SilentTrack, Track, clean_staging, install,
};
pub use kiln_core::ArtifactFormat;
pub use lockfile::{
    LOCKFILE_VERSION, LockedArtifact, LockedProject, LockedRuntime, Lockfile, PlatformLock,
};
pub use plan::{PlannedRuntime, plan};
pub use resolve::{Resolution, ResolvedRuntime, record, record_all, resolve};
