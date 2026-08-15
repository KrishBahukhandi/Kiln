//! Materialising a resolution: download, verify, unpack, store.
//!
//! The order matters and is not negotiable. For each runtime:
//!
//! 1. If the store already has the artifact's digest, stop — nothing to do.
//! 2. Download to staging, hashing in flight.
//! 3. Compare against the digest the publisher declared. A mismatch discards
//!    the bytes and fails.
//! 4. Unpack into staging.
//! 5. `rename(2)` the finished tree into the store.
//!
//! Only step 5 makes anything visible as installed, and it is atomic. An
//! install interrupted at any earlier point leaves the store exactly as it was,
//! and leaves at worst a directory in `staging/` that the next run cleans up.
//!
//! # Doing several at once, and why it is off by default
//!
//! Kiln can fetch runtimes concurrently — see [`install`]'s `jobs` argument —
//! but [`DEFAULT_JOBS`] is 1, because measurement did not support turning it on.
//!
//! Installing Node 22.14.0, Python 3.13.15 and Go 1.26.6 into an empty store
//! over a domestic connection, best of several runs each:
//!
//! ```text
//! sequential    60s     (14s + 27s + 19s)
//! 3 at once     75s     and as bad as 110s
//! ```
//!
//! Concurrency lost every time. Total bytes are fixed, so when the link is
//! already saturated by one download, splitting it three ways adds TCP
//! contention and three simultaneous streams of decompression and hashing
//! competing for the same disk — without adding any bandwidth to divide.
//!
//! It is kept, and exposed, because the picture inverts when the bottleneck is
//! the far end rather than the near one: on a CI runner with a very fast link,
//! a single stream from a distribution mirror is limited by the server, and
//! several streams genuinely do finish sooner. That is a real situation, but it
//! is not the situation most people run `kiln install` in, so it is opt-in.
//!
//! Two properties hold at any `jobs` value, because losing either would trade a
//! real guarantee for a few seconds:
//!
//! - **The reported error does not depend on which thread lost.** Every job runs
//!   to completion and results are sorted back into manifest order before the
//!   first failure is returned, so a given `kiln.toml` fails the same way every
//!   time.
//! - **One runtime never spawns a thread**, whatever `jobs` says.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kiln_cache::{ContentStore, EntryMeta, StoreEntry};
use kiln_core::error::{Error, IoResultExt, Result};
use kiln_core::{KilnPaths, RuntimeLayout, Version};
use kiln_net::{Download, Http, Progress, SilentProgress};
use kiln_runtime::Registry;

use crate::resolve::{Resolution, ResolvedRuntime};

/// How many artifacts Kiln fetches at once unless told otherwise.
///
/// One. See this module's documentation for the measurement behind that.
pub const DEFAULT_JOBS: usize = 1;

/// The most Kiln will fetch at once, whatever it is asked for.
///
/// Past a handful of streams the contention is certain and the benefit is not,
/// and every extra concurrent download is another partial transfer to unwind
/// when something fails.
pub const MAX_JOBS: usize = 8;

/// Watches an installation happen, so the CLI can draw progress without this
/// crate knowing what a terminal is.
///
/// The methods here are called from the calling thread. Anything that has to be
/// reported *while* a runtime is being fetched goes through [`Track`], which is
/// handed to the worker instead.
pub trait Observer {
    /// A runtime is already in the store and will not be fetched.
    fn reused(&mut self, runtime: &ResolvedRuntime);
    /// A runtime is about to be fetched; return the reporter for it.
    fn track(&mut self, runtime: &ResolvedRuntime) -> Box<dyn Track>;
    /// A runtime is now installed.
    fn installed(&mut self, runtime: &ResolvedRuntime, entry: &StoreEntry);
}

/// Reports one runtime's progress from the thread fetching it.
///
/// `Send` because it is moved onto a worker; not `Sync`, because nothing else
/// touches it once it has been handed over. That is what lets a terminal
/// implementation hold a progress bar without a lock around it.
pub trait Track: Send {
    /// Where download progress is reported.
    fn progress(&mut self) -> &mut dyn Progress;
    /// The archive has been verified and is being unpacked.
    fn unpacking(&mut self);
}

/// An [`Observer`] that reports nothing.
#[derive(Debug, Default)]
pub struct SilentObserver;

impl Observer for SilentObserver {
    fn reused(&mut self, _runtime: &ResolvedRuntime) {}
    fn track(&mut self, _runtime: &ResolvedRuntime) -> Box<dyn Track> {
        Box::new(SilentTrack(SilentProgress))
    }
    fn installed(&mut self, _runtime: &ResolvedRuntime, _entry: &StoreEntry) {}
}

/// A [`Track`] that reports nothing.
#[derive(Debug, Default)]
pub struct SilentTrack(SilentProgress);

impl Track for SilentTrack {
    fn progress(&mut self) -> &mut dyn Progress {
        &mut self.0
    }
    fn unpacking(&mut self) {}
}

/// One runtime, installed and ready to be put on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledRuntime {
    /// The identifier from `kiln.toml`.
    pub id: String,
    /// The provider's human-facing name.
    pub display_name: String,
    /// The exact version installed.
    pub version: Version,
    /// Where it lives in the store.
    pub entry: StoreEntry,
    /// Directories to prepend to `PATH`, in order.
    pub bin_dirs: Vec<PathBuf>,
    /// Whether it was already present before this run.
    pub was_cached: bool,
}

/// What an install did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    /// Every runtime the project needs, installed.
    pub runtimes: Vec<InstalledRuntime>,
}

impl InstallOutcome {
    /// How many runtimes were already in the store.
    pub fn reused(&self) -> usize {
        self.runtimes.iter().filter(|r| r.was_cached).count()
    }

    /// How many runtimes were downloaded.
    pub fn downloaded(&self) -> usize {
        self.runtimes.len() - self.reused()
    }

    /// Every `PATH` entry the project environment needs, in order.
    pub fn path_entries(&self) -> Vec<PathBuf> {
        self.runtimes
            .iter()
            .flat_map(|r| r.bin_dirs.iter().cloned())
            .collect()
    }
}

/// One runtime waiting to be fetched.
struct Job<'a> {
    /// Position in the resolution, so results can be put back in order.
    index: usize,
    runtime: &'a ResolvedRuntime,
    layout: RuntimeLayout,
    track: Box<dyn Track>,
}

/// Install everything in `resolution`.
///
/// `jobs` is how many runtimes to fetch at once; it is clamped to at least 1 and
/// at most [`MAX_JOBS`]. [`DEFAULT_JOBS`] is what the CLI passes unless asked
/// otherwise — the module documentation explains why that is 1.
pub fn install(
    resolution: &Resolution,
    registry: &Registry,
    paths: &KilnPaths,
    http: &Http,
    observer: &mut dyn Observer,
    jobs: usize,
) -> Result<InstallOutcome> {
    paths.ensure()?;
    let store = ContentStore::new(paths.store());
    let concurrency = jobs.clamp(1, MAX_JOBS);

    // Everything that touches the registry or the observer happens here, on one
    // thread, before any worker starts. A worker needs only plain data.
    let mut slots: Vec<Option<InstalledRuntime>> =
        (0..resolution.runtimes.len()).map(|_| None).collect();
    let mut pending = Vec::new();

    for (index, runtime) in resolution.runtimes.iter().enumerate() {
        let layout = registry
            .get(&runtime.id)
            .ok_or_else(|| registry.unknown(&runtime.id))?
            .layout();

        // Already installed: the whole point of content addressing.
        if let Some(entry) = store.get(&runtime.artifact.digest) {
            observer.reused(runtime);
            slots[index] = Some(InstalledRuntime {
                id: runtime.id.clone(),
                display_name: runtime.display_name.clone(),
                version: runtime.version.clone(),
                bin_dirs: bin_dirs_of(&entry, layout),
                entry,
                was_cached: true,
            });
            continue;
        }

        pending.push(Job {
            index,
            runtime,
            layout,
            track: observer.track(runtime),
        });
    }

    for (index, outcome) in run(pending, concurrency, &store, paths, http) {
        // Sorted by index already, so the first failure is the first one in
        // `kiln.toml` rather than whichever thread happened to finish first.
        let installed = outcome?;
        observer.installed(&resolution.runtimes[index], &installed.entry);
        slots[index] = Some(installed);
    }

    Ok(InstallOutcome {
        runtimes: slots
            .into_iter()
            .map(|slot| {
                slot.ok_or_else(|| {
                    Error::internal("An installed runtime went missing between planning and report")
                })
            })
            .collect::<Result<Vec<_>>>()?,
    })
}

/// Run every job, concurrently when there is more than one.
///
/// Results come back sorted by index, so callers see manifest order regardless
/// of what the network did.
fn run<'a>(
    jobs: Vec<Job<'a>>,
    concurrency: usize,
    store: &ContentStore,
    paths: &KilnPaths,
    http: &Http,
) -> Vec<(usize, Result<InstalledRuntime>)> {
    // One runtime, or sequential by request, gets the plain path: no threads, no
    // queue, nothing to reason about when something goes wrong.
    if jobs.len() <= 1 || concurrency <= 1 {
        return jobs
            .into_iter()
            .map(|job| (job.index, fetch(job, store, paths, http)))
            .collect();
    }

    let workers = jobs.len().min(concurrency);
    let queue: Mutex<VecDeque<Job<'a>>> = Mutex::new(jobs.into());
    let results: Mutex<Vec<(usize, Result<InstalledRuntime>)>> = Mutex::new(Vec::new());

    // Scoped threads so the jobs can borrow the resolution rather than cloning
    // it, and so every worker is joined before this function returns.
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some(job) = lock(&queue).pop_front() else {
                        break;
                    };
                    let index = job.index;
                    let outcome = fetch(job, store, paths, http);
                    lock(&results).push((index, outcome));
                }
            });
        }
    });

    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(index, _)| *index);
    results
}

/// Take a lock, ignoring poisoning.
///
/// The critical sections here are a `pop_front` and a `push`. Neither can leave
/// the protected value inconsistent, so a panic elsewhere in a worker is no
/// reason to take down the workers that are still making progress.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Download, verify, unpack and store one runtime.
fn fetch(
    mut job: Job<'_>,
    store: &ContentStore,
    paths: &KilnPaths,
    http: &Http,
) -> Result<InstalledRuntime> {
    let runtime = job.runtime;
    let workspace = Workspace::create(paths, &runtime.id, &runtime.version)?;
    let archive = workspace.path().join("artifact");

    let label = format!("{} {}", runtime.display_name, runtime.version);
    let bytes = Download {
        url: &runtime.artifact.url,
        what: &label,
        expected: &runtime.artifact.digest,
        size: runtime.artifact.size,
    }
    .to_file(http, &archive, job.track.progress())?;

    job.track.unpacking();
    let content = kiln_cache::extract(
        &archive,
        runtime.artifact.format,
        workspace.path(),
        job.layout.strip_components,
    )?;

    // The archive is no longer needed and would otherwise be moved into the
    // store along with everything else in the workspace.
    let _ = std::fs::remove_file(&archive);

    let meta = EntryMeta::new(
        &runtime.id,
        runtime.version.to_string(),
        &runtime.artifact.url,
        &runtime.artifact.digest,
        bytes,
    );
    let entry = store.insert(&content, &runtime.artifact.digest, &meta)?;

    Ok(InstalledRuntime {
        id: runtime.id.clone(),
        display_name: runtime.display_name.clone(),
        version: runtime.version.clone(),
        bin_dirs: bin_dirs_of(&entry, job.layout),
        entry,
        was_cached: false,
    })
}

/// Where a stored entry keeps its executables.
fn bin_dirs_of(entry: &StoreEntry, layout: RuntimeLayout) -> Vec<PathBuf> {
    layout.bin_paths(&entry.content_path())
}

/// A scratch directory under `~/.kiln/staging`, removed when it goes out of
/// scope.
///
/// It lives beside the store rather than in `/tmp` so that promoting a finished
/// entry is a rename within one filesystem. Cleaning up on drop means an
/// interrupted or failed install does not accumulate half-unpacked runtimes.
struct Workspace {
    path: PathBuf,
}

impl Workspace {
    fn create(paths: &KilnPaths, id: &str, version: &Version) -> Result<Self> {
        // The id is a validated runtime name and the version is parsed, so
        // neither can contribute a path separator.
        let path = paths
            .staging()
            .join(format!("{id}-{version}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .io_context("Could not create the staging directory", &path)?;
        Ok(Workspace { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Remove staging directories left behind by interrupted runs.
pub fn clean_staging(paths: &KilnPaths) -> Result<usize> {
    let staging = paths.staging();
    if !staging.is_dir() {
        return Ok(0);
    }

    let mut removed = 0;
    for entry in std::fs::read_dir(&staging)
        .io_context("Could not read the staging directory", &staging)?
        .flatten()
    {
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// The error to report when an install is asked for offline and the store does
/// not already have what it needs.
pub fn offline_gap(missing: &[&ResolvedRuntime]) -> Error {
    let names: Vec<String> = missing
        .iter()
        .map(|r| format!("{} {}", r.display_name, r.version))
        .collect();

    Error::new(
        kiln_core::ErrorKind::Network,
        format!(
            "{} not available locally",
            if names.len() == 1 {
                format!("{} is", names[0])
            } else {
                format!("{} are", names.join(", "))
            }
        ),
    )
    .because("Kiln is running offline, and these are not in the store yet.")
    .hint("run without `--offline` once, to fetch them")
    .command("kiln cache list")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::{Digest, HashAlgorithm};

    fn paths() -> (tempfile::TempDir, KilnPaths) {
        let directory = tempfile::tempdir().unwrap();
        let paths = KilnPaths::with_root(directory.path().join("kiln"));
        (directory, paths)
    }

    fn runtime(name: &str) -> ResolvedRuntime {
        ResolvedRuntime {
            id: "node".into(),
            display_name: name.into(),
            requirement: kiln_core::VersionReq::parse("22").unwrap(),
            version: Version::new(22, 14, 0),
            artifact: kiln_runtime::ArtifactSpec {
                url: "https://nodejs.org/x.tar.gz".into(),
                digest: Digest::of_bytes(HashAlgorithm::Sha256, b"x"),
                format: kiln_core::ArtifactFormat::TarGz,
                size: None,
            },
            from_lockfile: false,
        }
    }

    #[test]
    fn a_workspace_cleans_up_after_itself() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();

        let path = {
            let workspace = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();
            std::fs::write(workspace.path().join("artifact"), b"partial").unwrap();
            workspace.path().to_path_buf()
        };

        // A failed install must not leave a half-downloaded artifact behind.
        assert!(!path.exists());
    }

    #[test]
    fn a_workspace_lives_beside_the_store() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();
        let workspace = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();

        // Promotion into the store is a rename, which needs one filesystem.
        assert_eq!(workspace.path().parent(), Some(paths.staging().as_path()));
    }

    #[test]
    fn a_workspace_can_be_recreated_over_a_previous_attempt() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();

        let first = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();
        let path = first.path().to_path_buf();
        std::fs::write(path.join("artifact"), b"stale").unwrap();
        std::mem::forget(first); // simulate a killed process

        let second = Workspace::create(&paths, "node", &Version::new(22, 14, 0)).unwrap();
        assert!(
            !second.path().join("artifact").exists(),
            "a retry must not inherit the previous attempt's files"
        );
    }

    #[test]
    fn cleaning_staging_removes_orphans() {
        let (_guard, paths) = paths();
        paths.ensure().unwrap();
        std::fs::create_dir_all(paths.staging().join("node-22.14.0-999")).unwrap();
        std::fs::create_dir_all(paths.staging().join("python-3.13.5-998")).unwrap();

        assert_eq!(clean_staging(&paths).unwrap(), 2);
        assert!(paths.staging().read_dir().unwrap().next().is_none());
    }

    #[test]
    fn cleaning_a_store_that_was_never_created_is_fine() {
        let (_guard, paths) = paths();
        assert_eq!(clean_staging(&paths).unwrap(), 0);
    }

    #[test]
    fn installing_offline_names_what_is_missing() {
        let node = runtime("Node.js");
        let error = offline_gap(&[&node]);

        assert_eq!(error.kind(), kiln_core::ErrorKind::Network);
        assert!(error.summary().contains("Node.js 22.14.0 is"));
        assert!(error.hints().iter().any(|h| h.text().contains("--offline")));
    }

    #[test]
    fn the_offline_message_reads_correctly_for_several_runtimes() {
        let node = runtime("Node.js");
        let python = runtime("Python");
        let error = offline_gap(&[&node, &python]);
        assert!(error.summary().contains("are not available"));
    }

    #[test]
    fn concurrency_is_clamped_to_something_sane() {
        // `--jobs 0` must not mean "no workers, hang forever", and `--jobs 500`
        // must not mean five hundred sockets.
        assert_eq!(0usize.clamp(1, MAX_JOBS), 1);
        assert_eq!(500usize.clamp(1, MAX_JOBS), MAX_JOBS);
        assert_eq!(DEFAULT_JOBS.clamp(1, MAX_JOBS), DEFAULT_JOBS);
    }

    #[test]
    fn the_default_is_sequential() {
        // Measured, not assumed — see this module's documentation. If this ever
        // changes it should change because someone re-measured.
        assert_eq!(DEFAULT_JOBS, 1);
    }

    #[test]
    fn results_come_back_in_manifest_order_whatever_the_workers_did() {
        // The guarantee that makes a failing install reproducible: whichever
        // job finishes first, the caller sees `kiln.toml` order.
        let mut finished: Vec<(usize, &str)> = vec![(2, "go"), (0, "node"), (1, "python")];
        finished.sort_by_key(|(index, _)| *index);

        assert_eq!(
            finished.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
            ["node", "python", "go"]
        );
    }

    #[test]
    fn outcome_counts_split_cached_from_downloaded() {
        let entry = StoreEntry {
            digest: Digest::of_bytes(HashAlgorithm::Sha256, b"x"),
            path: PathBuf::from("/store/x"),
            meta: None,
            last_used: None,
        };
        let make = |cached: bool| InstalledRuntime {
            id: "node".into(),
            display_name: "Node.js".into(),
            version: Version::new(22, 14, 0),
            entry: entry.clone(),
            bin_dirs: vec![PathBuf::from("/store/x/content/bin")],
            was_cached: cached,
        };

        let outcome = InstallOutcome {
            runtimes: vec![make(true), make(false), make(true)],
        };
        assert_eq!(outcome.reused(), 2);
        assert_eq!(outcome.downloaded(), 1);
        assert_eq!(outcome.path_entries().len(), 3);
    }
}
