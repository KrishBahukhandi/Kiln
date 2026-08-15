//! The on-disk store: where an artifact lives, and what is already there.
//!
//! An entry is a directory named by the digest of the **archive it came from**:
//!
//! ```text
//! store/sha256/9f/9f86d081…/
//!   meta.toml       provider, version, and the URL it was fetched from
//!   tree.manifest   every path in `content/`, as unpacked
//!   last-used       when anything last needed this entry
//!   content/        the unpacked runtime
//! ```
//!
//! Naming an entry by its source archive rather than by a hash of the unpacked
//! tree is what makes this work without a canonical directory-hashing scheme.
//! The archive's digest is published and signed for by the vendor; the unpacked
//! tree is a deterministic function of it. Kiln verifies the thing upstream
//! actually attests to.
//!
//! The consequence is that an entry's name cannot say whether the tree still
//! matches, since the archive is gone by then. `tree.manifest` answers that
//! instead — see [`crate::tree`].
//!
//! Everything except `content/` sits beside the runtime rather than inside it,
//! so Kiln can record what it needs without contaminating the runtime with
//! files it did not ship.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use kiln_core::error::{Error, IoResultExt, Result};
use kiln_core::{Digest, HashAlgorithm};
use serde::{Deserialize, Serialize};

use crate::tree::{Difference, TreeManifest, manifest_of};

/// Number of leading hex characters used as a fan-out directory.
///
/// Two characters give 256 buckets, which keeps directory listings small enough
/// for filesystems that degrade on very wide directories without burying entries
/// so deep that the store becomes tedious to inspect by hand.
const SHARD_LENGTH: usize = 2;

/// The unpacked runtime, inside an entry directory.
const CONTENT_DIR: &str = "content";
/// Provenance, inside an entry directory.
const META_FILE: &str = "meta.toml";
/// When this entry was last put to use, inside an entry directory.
const LAST_USED_FILE: &str = "last-used";
/// What the unpacked tree looked like on arrival, inside an entry directory.
///
/// Beside `content/` rather than in it, for the same reason as `meta.toml`: a
/// runtime must not gain files it did not ship.
const TREE_FILE: &str = "tree.manifest";

/// How stale a recorded use has to be before Kiln rewrites it.
///
/// Bounds the cost of tracking use to one tiny write per entry per hour, no
/// matter how often `kiln run` is called. Garbage collection works in days, so
/// an hour of imprecision is free.
const TOUCH_INTERVAL_SECS: u64 = 60 * 60;

/// What Kiln records about how an entry got here.
///
/// None of it is needed to *use* the entry — the digest already identifies it —
/// but a store you cannot explain is a store nobody trusts enough to keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMeta {
    /// The provider that installed it, e.g. `node`.
    pub provider: String,
    /// The version installed.
    pub version: String,
    /// Where the artifact was fetched from.
    pub url: String,
    /// The artifact's digest, repeated here so a copied entry stays traceable.
    pub digest: String,
    /// Size of the downloaded archive, in bytes.
    pub artifact_bytes: u64,
    /// When it was installed, in seconds since the Unix epoch.
    ///
    /// A bare integer rather than a formatted timestamp, so the store needs no
    /// date library and no timezone to be read back.
    pub installed_unix: u64,

    /// The digest of this entry's [`TreeManifest`], standing for the whole
    /// unpacked tree.
    ///
    /// Optional because entries installed before Kiln recorded manifests have
    /// none, and are reported as unverifiable rather than as damaged. Defaulted
    /// on read so an old `meta.toml` still parses.
    #[serde(default)]
    pub tree_digest: Option<String>,
}

impl EntryMeta {
    /// Record an installation happening now.
    pub fn new(
        provider: impl Into<String>,
        version: impl Into<String>,
        url: impl Into<String>,
        digest: &Digest,
        artifact_bytes: u64,
    ) -> Self {
        EntryMeta {
            provider: provider.into(),
            version: version.into(),
            url: url.into(),
            digest: digest.to_string(),
            artifact_bytes,
            installed_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            tree_digest: None,
        }
    }
}

/// One artifact in the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreEntry {
    /// The content address of the artifact this was unpacked from.
    pub digest: Digest,
    /// The entry directory.
    pub path: PathBuf,
    /// Provenance, when it could be read.
    pub meta: Option<EntryMeta>,
    /// When this entry was last used, in seconds since the Unix epoch.
    ///
    /// `None` for an entry installed before Kiln tracked this, which garbage
    /// collection treats as "installed long ago" rather than "never used".
    pub last_used: Option<u64>,
}

impl StoreEntry {
    /// The unpacked runtime — what goes on `PATH`.
    pub fn content_path(&self) -> PathBuf {
        self.path.join(CONTENT_DIR)
    }

    /// The best available answer to "when did anything last need this?".
    ///
    /// Falls back to the install time, so an entry from before use-tracking
    /// existed still ages rather than living forever.
    pub fn last_touched(&self) -> Option<u64> {
        self.last_used
            .or_else(|| self.meta.as_ref().map(|meta| meta.installed_unix))
    }

    /// How long ago this entry was last needed, in seconds.
    pub fn idle_seconds(&self, now: u64) -> Option<u64> {
        self.last_touched().map(|then| now.saturating_sub(then))
    }
}

/// What checking one entry found.
///
/// "Kiln could not tell" is a separate answer from "the entry is fine", and
/// keeping them apart is the point of this type. Collapsing them into a boolean
/// would make an unverifiable entry report as healthy, which is the one thing a
/// verification command must never do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    /// Every recorded path is present and unchanged.
    Intact {
        /// How many regular files were checked.
        files: usize,
        /// How many bytes were read.
        bytes: u64,
    },
    /// The entry exists, but there is nothing to check it against.
    Unverifiable {
        /// Why not, phrased to follow "Kiln cannot verify this entry because…".
        reason: String,
    },
    /// The tree no longer matches what was installed.
    Damaged {
        /// Every path that differs, in path order.
        differences: Vec<Difference>,
    },
}

impl Verification {
    /// Whether this entry is known to be good.
    pub fn is_intact(&self) -> bool {
        matches!(self, Verification::Intact { .. })
    }

    /// Whether this entry is known to be bad.
    pub fn is_damaged(&self) -> bool {
        matches!(self, Verification::Damaged { .. })
    }
}

/// The content-addressed store rooted at a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentStore {
    root: PathBuf,
}

impl ContentStore {
    /// Open the store rooted at `root`, which is normally `~/.kiln/store`.
    ///
    /// Opening does not touch the filesystem: commands that only read should not
    /// create directories as a side effect.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ContentStore { root: root.into() }
    }

    /// The store's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where an artifact with this digest belongs.
    ///
    /// This is a pure function of the digest. Path traversal is structurally
    /// impossible: a [`Digest`] only parses from lowercase hex, so no component
    /// derived from it can contain a separator or `..`.
    pub fn path_for(&self, digest: &Digest) -> PathBuf {
        let hex = digest.hex();
        let (shard, _) = hex.split_at(SHARD_LENGTH);
        self.root
            .join(digest.algorithm().as_str())
            .join(shard)
            .join(&hex)
    }

    /// Where the unpacked runtime for this digest belongs.
    pub fn content_path_for(&self, digest: &Digest) -> PathBuf {
        self.path_for(digest).join(CONTENT_DIR)
    }

    /// Whether this artifact is already installed.
    ///
    /// Presence of the `content` directory is the test, not the entry directory:
    /// an entry that exists without content is a failed install, and must not be
    /// mistaken for a complete one.
    pub fn contains(&self, digest: &Digest) -> bool {
        self.content_path_for(digest).is_dir()
    }

    /// Look an artifact up.
    pub fn get(&self, digest: &Digest) -> Option<StoreEntry> {
        if !self.contains(digest) {
            return None;
        }
        let path = self.path_for(digest);
        Some(StoreEntry {
            digest: digest.clone(),
            meta: read_meta(&path),
            last_used: read_last_used(&path),
            path,
        })
    }

    /// Record that this entry was just put to use.
    ///
    /// Kiln tracks use itself rather than reading the filesystem's access time,
    /// which is unreliable in practice: `relatime` is the default on Linux and
    /// `noatime` is common, so `atime` can be hours stale or frozen entirely.
    /// Garbage collection that deleted a runtime someone uses daily would be
    /// worse than no garbage collection.
    ///
    /// Best effort and never fatal — a read-only store, a full disk, or a
    /// concurrent Kiln are all fine reasons for this to do nothing. The cost of
    /// losing a timestamp is that an entry looks staler than it is, which at
    /// worst means re-downloading it.
    pub fn touch(&self, digest: &Digest) {
        let path = self.path_for(digest);
        if !path.is_dir() {
            return;
        }

        let now = unix_now();
        if let Some(recorded) = read_last_used(&path)
            && now.saturating_sub(recorded) < TOUCH_INTERVAL_SECS
        {
            return;
        }
        let _ = std::fs::write(path.join(LAST_USED_FILE), now.to_string());
    }

    /// Record use for several entries at once.
    pub fn touch_all<'a>(&self, digests: impl IntoIterator<Item = &'a Digest>) {
        for digest in digests {
            self.touch(digest);
        }
    }

    /// Move an unpacked runtime into the store under `digest`.
    ///
    /// `content` must be a directory on the same filesystem as the store, which
    /// is why staging lives under `~/.kiln` rather than `/tmp`: the final step is
    /// a `rename(2)`, and a rename is the only way to make an entry appear
    /// complete-or-not-at-all.
    ///
    /// Idempotent, and safe against a concurrent Kiln installing the same
    /// artifact: whoever loses the race discards their copy and uses the winner's.
    pub fn insert(&self, content: &Path, digest: &Digest, meta: &EntryMeta) -> Result<StoreEntry> {
        if let Some(existing) = self.get(digest) {
            let _ = std::fs::remove_dir_all(content);
            return Ok(existing);
        }

        let target = self.path_for(digest);
        let parent = target
            .parent()
            .ok_or_else(|| Error::internal("The store path has no parent directory"))?;
        std::fs::create_dir_all(parent)
            .io_context("Could not create the store directory", parent)?;

        // Assemble the finished entry beside the content, then move the whole
        // thing in one step.
        let staging = content.parent().unwrap_or(Path::new(".")).join(format!(
            ".entry-{}-{}",
            digest.short(),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)
            .io_context("Could not stage the store entry", &staging)?;

        let staged_content = staging.join(CONTENT_DIR);
        std::fs::rename(content, &staged_content).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            Error::io("Could not stage the unpacked runtime", content, e)
        })?;

        // Record what the tree looks like *now*, while the archive it came from
        // has just been verified. A manifest taken at any later moment would
        // only be able to attest that the tree matches itself.
        let manifest = manifest_of(&staged_content).inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&staging);
        })?;
        std::fs::write(staging.join(TREE_FILE), manifest.render())
            .io_context("Could not record the runtime's file manifest", &staging)?;

        let mut meta = meta.clone();
        meta.tree_digest = Some(manifest.digest().to_string());

        let rendered = toml::to_string_pretty(&meta)
            .map_err(|e| Error::internal("Could not record the store entry").with_source(e))?;
        std::fs::write(staging.join(META_FILE), rendered)
            .io_context("Could not record the store entry", &staging)?;

        match std::fs::rename(&staging, &target) {
            Ok(()) => {}
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                // Another process finished first. That is a success, not a
                // failure: the store is content-addressed, so their copy and
                // ours are the same bytes.
                if let Some(existing) = self.get(digest) {
                    return Ok(existing);
                }
                return Err(Error::io(
                    "Could not add the runtime to the store",
                    &target,
                    e,
                ));
            }
        }

        Ok(StoreEntry {
            digest: digest.clone(),
            meta: Some(meta.clone()),
            last_used: None,
            path: target,
        })
    }

    /// Every artifact currently in the store, ordered by digest.
    ///
    /// Directory entries whose names are not valid digests are skipped rather
    /// than reported: the store is a shared directory on a real machine, and a
    /// stray `.DS_Store` is not a reason to fail a command.
    pub fn entries(&self) -> Result<Vec<StoreEntry>> {
        let mut entries = Vec::new();
        if !self.root.is_dir() {
            return Ok(entries);
        }

        for algorithm in read_dir_sorted(&self.root)? {
            let Some(algorithm_name) = algorithm.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if algorithm_name.parse::<HashAlgorithm>().is_err() {
                continue;
            }
            for shard in read_dir_sorted(&algorithm)? {
                for entry in read_dir_sorted(&shard)? {
                    if !entry.is_dir() {
                        continue;
                    }
                    let Some(name) = entry.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    let Ok(digest) = Digest::parse(&format!("{algorithm_name}:{name}")) else {
                        continue;
                    };
                    // Guard against a hand-made directory in the wrong bucket,
                    // and against a half-written entry with no content.
                    if self.path_for(&digest) != entry || !entry.join(CONTENT_DIR).is_dir() {
                        continue;
                    }
                    entries.push(StoreEntry {
                        digest,
                        meta: read_meta(&entry),
                        last_used: read_last_used(&entry),
                        path: entry,
                    });
                }
            }
        }
        entries.sort_by(|a, b| a.digest.cmp(&b.digest));
        Ok(entries)
    }

    /// How much disk one entry occupies, in bytes.
    pub fn entry_size(&self, digest: &Digest) -> u64 {
        directory_size(&self.path_for(digest)).unwrap_or(0)
    }

    /// Total size of the store's contents, in bytes.
    pub fn size_on_disk(&self) -> Result<u64> {
        let mut total = 0;
        for entry in self.entries()? {
            total += directory_size(&entry.path)?;
        }
        Ok(total)
    }

    /// Re-walk a stored entry and compare it against the manifest recorded when
    /// it was installed.
    ///
    /// Reads every byte of the entry, because a digest is the only thing that
    /// separates a corrupted file from an intact one of the same length.
    ///
    /// Returns `Ok` for a damaged entry as well as an intact one: damage is a
    /// finding this command exists to report, not a failure to perform it. The
    /// `Err` cases are the ones where Kiln could not look — an unreadable
    /// directory, an unparseable manifest.
    pub fn verify(&self, digest: &Digest) -> Result<Verification> {
        let path = self.path_for(digest);
        if !self.contains(digest) {
            return Err(
                Error::not_found(format!("The store has no entry {}", digest.short()))
                    .because("Nothing is installed under that digest.")
                    .command("kiln cache list"),
            );
        }

        let manifest_path = path.join(TREE_FILE);
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            return Ok(Verification::Unverifiable {
                reason: "it was installed before Kiln recorded file manifests".into(),
            });
        };

        let recorded = TreeManifest::parse(&text)?;

        // The manifest is not a signature — it lives beside what it describes —
        // but checking it against the digest in `meta.toml` still catches the
        // manifest itself being truncated, which would otherwise show up as a
        // tree full of missing files.
        if let Some(expected) = read_meta(&path).and_then(|meta| meta.tree_digest)
            && recorded.digest().to_string() != expected
        {
            return Ok(Verification::Unverifiable {
                reason: "its file manifest does not match the digest recorded for it".into(),
            });
        }

        let actual = manifest_of(&path.join(CONTENT_DIR))?;
        let differences = recorded.compare(&actual);

        Ok(if differences.is_empty() {
            Verification::Intact {
                files: recorded.file_count(),
                bytes: recorded.total_bytes(),
            }
        } else {
            Verification::Damaged { differences }
        })
    }

    /// Delete one entry.
    pub fn remove(&self, digest: &Digest) -> Result<u64> {
        let path = self.path_for(digest);
        if !path.is_dir() {
            return Ok(0);
        }
        let size = directory_size(&path).unwrap_or(0);

        // Renamed out of the store first, so a concurrent reader walking the
        // tree never sees an entry mid-deletion. Once the rename lands the
        // entry is gone from the store's point of view, whether or not the
        // recursive delete that follows finishes.
        let condemned = path.with_file_name(format!(
            ".removing-{}-{}",
            digest.short(),
            std::process::id()
        ));
        match std::fs::rename(&path, &condemned) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&condemned);
            }
            // Losing a race with another Kiln removing the same entry is a
            // success: it is gone either way.
            Err(_) if !path.exists() => return Ok(0),
            Err(e) => return Err(Error::io("Could not remove the store entry", &path, e)),
        }
        Ok(size)
    }
}

fn read_meta(entry: &Path) -> Option<EntryMeta> {
    let text = std::fs::read_to_string(entry.join(META_FILE)).ok()?;
    toml::from_str(&text).ok()
}

fn read_last_used(entry: &Path) -> Option<u64> {
    std::fs::read_to_string(entry.join(LAST_USED_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// List a directory's children, sorted, so enumeration is deterministic.
fn read_dir_sorted(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(directory)
        .io_context("Could not read the artifact store", directory)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    Ok(paths)
}

fn directory_size(directory: &Path) -> Result<u64> {
    let mut total = 0;
    for path in read_dir_sorted(directory)? {
        let metadata = std::fs::symlink_metadata(&path)
            .io_context("Could not measure the artifact store", &path)?;
        if metadata.is_dir() {
            total += directory_size(&path)?;
        } else {
            total += metadata.len();
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest_of(data: &[u8]) -> Digest {
        Digest::of_bytes(HashAlgorithm::Sha256, data)
    }

    fn meta_for(digest: &Digest) -> EntryMeta {
        EntryMeta::new(
            "node",
            "22.14.0",
            "https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-arm64.tar.gz",
            digest,
            45_678_901,
        )
    }

    /// A store plus a staging area on the same filesystem, as in `~/.kiln`.
    struct Fixture {
        _directory: tempfile::TempDir,
        store: ContentStore,
        staging: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let store = ContentStore::new(directory.path().join("store"));
            let staging = directory.path().join("staging");
            std::fs::create_dir_all(&staging).unwrap();
            Fixture {
                _directory: directory,
                store,
                staging,
            }
        }

        /// Build an unpacked runtime ready to be inserted.
        fn stage(&self, label: &str, payload: &[u8]) -> PathBuf {
            let content = self.staging.join(label);
            std::fs::create_dir_all(content.join("bin")).unwrap();
            std::fs::write(content.join("bin/node"), payload).unwrap();
            content
        }
    }

    #[test]
    fn paths_are_sharded_by_the_first_two_hex_characters() {
        let store = ContentStore::new("/home/dev/.kiln/store");
        let digest = digest_of(b"");
        let hex = digest.hex();

        assert_eq!(
            store.path_for(&digest),
            Path::new("/home/dev/.kiln/store/sha256")
                .join(&hex[..2])
                .join(&hex)
        );
        assert_eq!(
            store.content_path_for(&digest),
            store.path_for(&digest).join("content")
        );
    }

    #[test]
    fn paths_stay_inside_the_store() {
        let store = ContentStore::new("/home/dev/.kiln/store");
        for data in [&b""[..], b"node", b"python", b"../../etc/passwd"] {
            let path = store.path_for(&digest_of(data));
            assert!(path.starts_with(store.root()));
            assert!(
                path.components()
                    .all(|c| c != std::path::Component::ParentDir),
                "no component may be `..`"
            );
        }
    }

    #[test]
    fn insert_makes_the_runtime_available() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node-22.14.0");
        let content = fixture.stage("node", b"#!/bin/sh\n");

        assert!(!fixture.store.contains(&digest));

        let entry = fixture
            .store
            .insert(&content, &digest, &meta_for(&digest))
            .unwrap();

        assert!(fixture.store.contains(&digest));
        assert_eq!(entry.path, fixture.store.path_for(&digest));
        assert!(entry.content_path().join("bin/node").is_file());
        assert!(!content.exists(), "the staged copy is moved, not copied");
    }

    #[test]
    fn provenance_is_recorded_and_read_back() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node-22.14.0");
        let content = fixture.stage("node", b"x");

        fixture
            .store
            .insert(&content, &digest, &meta_for(&digest))
            .unwrap();

        let meta = fixture.store.get(&digest).unwrap().meta.expect("meta.toml");
        assert_eq!(meta.provider, "node");
        assert_eq!(meta.version, "22.14.0");
        assert_eq!(meta.digest, digest.to_string());
        assert!(meta.url.starts_with("https://nodejs.org/"));
        assert!(meta.installed_unix > 1_700_000_000);
    }

    #[test]
    fn metadata_sits_beside_the_runtime_not_inside_it() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node");
        let content = fixture.stage("node", b"x");
        let entry = fixture
            .store
            .insert(&content, &digest, &meta_for(&digest))
            .unwrap();

        assert!(entry.path.join("meta.toml").is_file());
        assert!(
            !entry.content_path().join("meta.toml").exists(),
            "the runtime must not be contaminated with Kiln's bookkeeping"
        );
    }

    #[test]
    fn inserting_the_same_digest_twice_is_idempotent() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node-22.14.0");

        let first = fixture
            .store
            .insert(&fixture.stage("a", b"x"), &digest, &meta_for(&digest))
            .unwrap();

        // A second install of the same artifact — the common case when two
        // projects pin the same runtime.
        let second_content = fixture.stage("b", b"x");
        let second = fixture
            .store
            .insert(&second_content, &digest, &meta_for(&digest))
            .unwrap();

        assert_eq!(first.path, second.path);
        assert_eq!(fixture.store.entries().unwrap().len(), 1);
        assert!(!second_content.exists(), "the redundant copy is cleaned up");
    }

    #[test]
    fn two_projects_needing_the_same_runtime_share_one_entry() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node-22.14.0-tarball");

        fixture
            .store
            .insert(
                &fixture.stage("project-a", b"x"),
                &digest,
                &meta_for(&digest),
            )
            .unwrap();
        fixture
            .store
            .insert(
                &fixture.stage("project-b", b"x"),
                &digest,
                &meta_for(&digest),
            )
            .unwrap();

        assert_eq!(fixture.store.entries().unwrap().len(), 1);
    }

    #[test]
    fn a_half_written_entry_is_not_mistaken_for_an_install() {
        let fixture = Fixture::new();
        let digest = digest_of(b"interrupted");

        // What an install killed between creating the directory and moving the
        // content would leave behind.
        let orphan = fixture.store.path_for(&digest);
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("meta.toml"), "provider = \"node\"\n").unwrap();

        assert!(!fixture.store.contains(&digest));
        assert!(fixture.store.get(&digest).is_none());
        assert!(fixture.store.entries().unwrap().is_empty());
    }

    #[test]
    fn an_interrupted_install_can_be_retried() {
        let fixture = Fixture::new();
        let digest = digest_of(b"retry");
        std::fs::create_dir_all(fixture.store.path_for(&digest)).unwrap();

        let entry = fixture
            .store
            .insert(&fixture.stage("node", b"x"), &digest, &meta_for(&digest))
            .unwrap();
        assert!(entry.content_path().join("bin/node").is_file());
    }

    #[test]
    fn lookup_reports_what_is_present() {
        let fixture = Fixture::new();
        let present = digest_of(b"present");
        let absent = digest_of(b"absent");

        fixture
            .store
            .insert(&fixture.stage("p", b"x"), &present, &meta_for(&present))
            .unwrap();

        assert!(fixture.store.contains(&present));
        assert!(!fixture.store.contains(&absent));
        assert_eq!(fixture.store.get(&present).unwrap().digest, present);
        assert!(fixture.store.get(&absent).is_none());
    }

    #[test]
    fn an_empty_or_missing_store_lists_nothing() {
        let fixture = Fixture::new();
        assert!(fixture.store.entries().unwrap().is_empty());
        assert!(
            ContentStore::new(fixture.staging.join("never-created"))
                .entries()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn enumeration_is_sorted_and_ignores_junk() {
        let fixture = Fixture::new();
        let store = &fixture.store;

        let mut expected: Vec<Digest> = (0..5u8).map(|i| digest_of(&[i])).collect();
        for (index, digest) in expected.iter().enumerate() {
            store
                .insert(
                    &fixture.stage(&format!("s{index}"), b"x"),
                    digest,
                    &meta_for(digest),
                )
                .unwrap();
        }
        expected.sort();

        // Things that are not artifacts.
        std::fs::write(store.root().join(".DS_Store"), b"").unwrap();
        std::fs::create_dir_all(store.root().join("md5/ab/whatever/content")).unwrap();
        std::fs::create_dir_all(store.root().join("sha256/zz/not-hex/content")).unwrap();
        let misfiled = store
            .root()
            .join("sha256/00")
            .join(digest_of(b"misfiled").hex());
        std::fs::create_dir_all(misfiled.join("content")).unwrap();

        let found: Vec<Digest> = store
            .entries()
            .unwrap()
            .into_iter()
            .map(|e| e.digest)
            .collect();
        assert_eq!(found, expected);
    }

    #[test]
    fn size_counts_the_content_and_the_metadata() {
        let fixture = Fixture::new();
        let digest = digest_of(b"sized");
        let content = fixture.staging.join("sized");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(content.join("payload"), [0u8; 1000]).unwrap();

        fixture
            .store
            .insert(&content, &digest, &meta_for(&digest))
            .unwrap();

        let size = fixture.store.size_on_disk().unwrap();
        assert!(size >= 1000, "content should be counted, got {size}");
        assert!(size < 4000, "only the entry should be counted, got {size}");
    }

    #[test]
    fn unreadable_metadata_does_not_hide_an_entry() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node");
        fixture
            .store
            .insert(&fixture.stage("node", b"x"), &digest, &meta_for(&digest))
            .unwrap();

        std::fs::write(
            fixture.store.path_for(&digest).join("meta.toml"),
            "!!! not toml",
        )
        .unwrap();

        // The runtime is still perfectly usable; only its provenance is lost.
        let entry = fixture.store.get(&digest).expect("entry is still present");
        assert!(entry.meta.is_none());
        assert!(entry.content_path().join("bin/node").is_file());
    }

    /// A fixture with one runtime installed, ready to be tampered with.
    fn installed() -> (Fixture, Digest) {
        let fixture = Fixture::new();
        let digest = digest_of(b"node");
        fixture
            .store
            .insert(
                &fixture.stage("n", b"#!/bin/sh\n"),
                &digest,
                &meta_for(&digest),
            )
            .unwrap();
        (fixture, digest)
    }

    #[test]
    fn a_freshly_installed_entry_verifies() {
        let (fixture, digest) = installed();
        let verification = fixture.store.verify(&digest).unwrap();

        assert_eq!(
            verification,
            Verification::Intact {
                files: 1,
                bytes: 10,
            }
        );
    }

    #[test]
    fn installing_records_a_manifest_beside_the_content() {
        let (fixture, digest) = installed();
        let entry = fixture.store.path_for(&digest);

        assert!(entry.join(TREE_FILE).is_file());
        // Beside, never inside: a runtime must not gain files it did not ship.
        assert!(!entry.join(CONTENT_DIR).join(TREE_FILE).exists());
    }

    #[test]
    fn the_manifest_digest_is_recorded_in_the_metadata() {
        let (fixture, digest) = installed();
        let meta = fixture.store.get(&digest).unwrap().meta.unwrap();
        let recorded = meta.tree_digest.expect("a fresh install records one");

        let text =
            std::fs::read_to_string(fixture.store.path_for(&digest).join(TREE_FILE)).unwrap();
        assert_eq!(
            TreeManifest::parse(&text).unwrap().digest().to_string(),
            recorded
        );
    }

    #[test]
    fn a_corrupted_file_is_reported_as_damage_not_as_an_error() {
        let (fixture, digest) = installed();
        let file = fixture.store.content_path_for(&digest).join("bin/node");

        // Same length, different bytes — what bit-rot looks like, and what a
        // size-only check would call healthy.
        std::fs::write(&file, b"#!/bin/SH\n").unwrap();

        let verification = fixture.store.verify(&digest).unwrap();
        let Verification::Damaged { differences } = verification else {
            panic!("corruption must be reported, not swallowed");
        };
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].path, "bin/node");
    }

    #[test]
    fn an_entry_installed_before_manifests_existed_is_unverifiable() {
        let (fixture, digest) = installed();
        std::fs::remove_file(fixture.store.path_for(&digest).join(TREE_FILE)).unwrap();

        // The one answer that must never collapse into "intact".
        let verification = fixture.store.verify(&digest).unwrap();
        assert!(!verification.is_intact());
        assert!(!verification.is_damaged());
    }

    #[test]
    fn a_truncated_manifest_is_unverifiable_rather_than_a_ruined_tree() {
        let (fixture, digest) = installed();
        let manifest = fixture.store.path_for(&digest).join(TREE_FILE);

        // Losing the body of the manifest would otherwise report every file in
        // the runtime as unexpected, which points at the wrong culprit.
        std::fs::write(&manifest, "kiln-tree 1\n").unwrap();

        let verification = fixture.store.verify(&digest).unwrap();
        assert!(matches!(verification, Verification::Unverifiable { .. }));
    }

    #[test]
    fn verifying_something_that_is_not_installed_says_so() {
        let fixture = Fixture::new();
        let error = fixture.store.verify(&digest_of(b"absent")).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::NotFound);
    }

    #[test]
    fn metadata_from_before_manifests_still_parses() {
        // An old `meta.toml` has no `tree_digest` key at all. Failing to read it
        // would make every pre-existing entry unusable, not merely unverifiable.
        let text = "provider = \"node\"\nversion = \"22.14.0\"\n\
                    url = \"https://nodejs.org/x.tar.gz\"\ndigest = \"sha256:ab\"\n\
                    artifact_bytes = 10\ninstalled_unix = 1700000000\n";
        let meta: EntryMeta = toml::from_str(text).unwrap();
        assert_eq!(meta.tree_digest, None);
    }

    #[test]
    fn use_is_recorded_and_read_back() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node");
        fixture
            .store
            .insert(&fixture.stage("n", b"x"), &digest, &meta_for(&digest))
            .unwrap();

        assert!(fixture.store.get(&digest).unwrap().last_used.is_none());

        fixture.store.touch(&digest);
        let entry = fixture.store.get(&digest).unwrap();
        assert!(entry.last_used.unwrap() > 1_700_000_000);
        assert!(entry.idle_seconds(unix_now()).unwrap() < 5);
    }

    #[test]
    fn touching_an_absent_entry_does_nothing() {
        let fixture = Fixture::new();
        fixture.store.touch(&digest_of(b"absent"));
        assert!(fixture.store.entries().unwrap().is_empty());
    }

    #[test]
    fn an_entry_never_touched_ages_from_its_install_time() {
        // Entries written before use-tracking existed must still age, or they
        // would live in the store forever.
        let fixture = Fixture::new();
        let digest = digest_of(b"old");
        fixture
            .store
            .insert(&fixture.stage("o", b"x"), &digest, &meta_for(&digest))
            .unwrap();

        let entry = fixture.store.get(&digest).unwrap();
        assert!(entry.last_used.is_none());
        assert_eq!(
            entry.last_touched(),
            Some(entry.meta.unwrap().installed_unix)
        );
    }

    #[test]
    fn recording_use_does_not_disturb_provenance() {
        let fixture = Fixture::new();
        let digest = digest_of(b"node");
        fixture
            .store
            .insert(&fixture.stage("n", b"x"), &digest, &meta_for(&digest))
            .unwrap();

        let before = fixture.store.get(&digest).unwrap().meta;
        fixture.store.touch(&digest);
        assert_eq!(fixture.store.get(&digest).unwrap().meta, before);
    }

    #[test]
    fn removing_an_entry_takes_it_out_of_the_store() {
        let fixture = Fixture::new();
        let digest = digest_of(b"doomed");
        let content = fixture.staging.join("doomed");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(content.join("payload"), [0u8; 2048]).unwrap();
        fixture
            .store
            .insert(&content, &digest, &meta_for(&digest))
            .unwrap();

        let freed = fixture.store.remove(&digest).unwrap();
        assert!(freed >= 2048, "should report what it freed, got {freed}");
        assert!(!fixture.store.contains(&digest));
        assert!(fixture.store.entries().unwrap().is_empty());
    }

    #[test]
    fn removing_leaves_no_debris_behind() {
        let fixture = Fixture::new();
        let digest = digest_of(b"doomed");
        fixture
            .store
            .insert(&fixture.stage("d", b"x"), &digest, &meta_for(&digest))
            .unwrap();
        fixture.store.remove(&digest).unwrap();

        let shard = fixture
            .store
            .path_for(&digest)
            .parent()
            .unwrap()
            .to_path_buf();
        let leftovers: Vec<_> = std::fs::read_dir(&shard)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert!(leftovers.is_empty(), "the shard should be empty");
    }

    #[test]
    fn removing_something_that_is_not_there_succeeds() {
        let fixture = Fixture::new();
        assert_eq!(fixture.store.remove(&digest_of(b"never")).unwrap(), 0);
    }

    #[test]
    fn removing_one_entry_leaves_the_others() {
        let fixture = Fixture::new();
        let keep = digest_of(b"keep");
        let drop = digest_of(b"drop");
        for (label, digest) in [("k", &keep), ("d", &drop)] {
            fixture
                .store
                .insert(&fixture.stage(label, b"x"), digest, &meta_for(digest))
                .unwrap();
        }

        fixture.store.remove(&drop).unwrap();
        assert!(fixture.store.contains(&keep));
        assert_eq!(fixture.store.entries().unwrap().len(), 1);
    }
}
