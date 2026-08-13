//! A time-to-live cache for release indexes.
//!
//! The Node.js release index is a quarter of a megabyte that changes a few times
//! a week. Re-fetching it for every project on a machine is wasteful, and it
//! makes Kiln feel slow for no reason.
//!
//! This cache is only ever used for *metadata*. Artifacts are not cached here —
//! they live in the content-addressed store, where they are named by their
//! digest rather than by a URL and an expiry time.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kiln_core::error::{IoResultExt, Result};
use kiln_core::{Digest, HashAlgorithm};

/// How long a cached release index stays fresh.
///
/// Long enough to make repeated installs across projects free, short enough that
/// a release published this morning is visible this afternoon. An exact pin does
/// not consult the index at all, and a lockfile skips resolution entirely, so
/// this only affects floating requirements being resolved for the first time.
pub const DEFAULT_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// A directory of cached HTTP responses, keyed by URL.
#[derive(Debug, Clone)]
pub struct MetadataCache {
    root: PathBuf,
    ttl: Duration,
}

impl MetadataCache {
    /// Cache responses under `root`, normally `~/.kiln/state/http`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        MetadataCache {
            root: root.into(),
            ttl: DEFAULT_TTL,
        }
    }

    /// Override the freshness window.
    #[must_use]
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Where a URL's response is stored.
    ///
    /// The filename is the digest of the URL rather than the URL itself: a URL
    /// contains `/`, `?` and `..`, none of which belong in a path component.
    fn path_for(&self, url: &str) -> PathBuf {
        self.root
            .join(Digest::of_bytes(HashAlgorithm::Sha256, url.as_bytes()).hex())
    }

    /// The cached response, if there is one and it is still fresh.
    ///
    /// Every failure is treated as a miss. A cache is an optimisation, and an
    /// unreadable one must never turn into a failed command.
    pub fn get(&self, url: &str) -> Option<String> {
        let path = self.path_for(url);
        let metadata = std::fs::metadata(&path).ok()?;
        let age = SystemTime::now()
            .duration_since(metadata.modified().ok()?)
            .ok()?;
        if age > self.ttl {
            return None;
        }
        std::fs::read_to_string(&path).ok()
    }

    /// Store a response.
    ///
    /// Written to a temporary file and renamed, so a concurrent reader sees
    /// either the old entry or the new one, never a half-written one.
    pub fn put(&self, url: &str, body: &str) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .io_context("Could not create the metadata cache", &self.root)?;

        let path = self.path_for(url);
        let temporary = self.root.join(format!(
            ".{}.{}.tmp",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("entry"),
            std::process::id()
        ));

        std::fs::write(&temporary, body)
            .io_context("Could not write to the metadata cache", &temporary)?;
        if std::fs::rename(&temporary, &path).is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        Ok(())
    }

    /// Forget everything cached.
    pub fn clear(&self) -> Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)
                .io_context("Could not clear the metadata cache", &self.root)?;
        }
        Ok(())
    }

    /// The directory this cache lives in.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("kiln-httpcache-{label}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn stores_and_returns_a_response() {
        let cache = MetadataCache::new(scratch("roundtrip"));
        assert!(cache.get("https://example.test/index.json").is_none());

        cache
            .put("https://example.test/index.json", "{\"a\":1}")
            .unwrap();
        assert_eq!(
            cache.get("https://example.test/index.json").as_deref(),
            Some("{\"a\":1}")
        );

        cache.clear().unwrap();
    }

    #[test]
    fn different_urls_do_not_collide() {
        let cache = MetadataCache::new(scratch("collide"));
        cache.put("https://example.test/a", "first").unwrap();
        cache.put("https://example.test/b", "second").unwrap();

        assert_eq!(
            cache.get("https://example.test/a").as_deref(),
            Some("first")
        );
        assert_eq!(
            cache.get("https://example.test/b").as_deref(),
            Some("second")
        );

        cache.clear().unwrap();
    }

    #[test]
    fn urls_never_become_path_components() {
        let cache = MetadataCache::new("/tmp/kiln-cache");
        let path = cache.path_for("https://example.test/../../etc/passwd?x=1");

        assert_eq!(path.parent(), Some(Path::new("/tmp/kiln-cache")));
        assert!(
            path.components()
                .all(|c| c != std::path::Component::ParentDir)
        );
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 64);
        assert!(name.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn stale_entries_are_a_miss() {
        let cache = MetadataCache::new(scratch("stale")).with_ttl(Duration::ZERO);
        cache.put("https://example.test/index.json", "old").unwrap();

        // With a zero-length freshness window, anything already written is stale.
        assert!(cache.get("https://example.test/index.json").is_none());
        cache.clear().unwrap();
    }

    #[test]
    fn an_unreadable_cache_is_a_miss_not_a_failure() {
        let cache = MetadataCache::new("/proc/nonexistent/kiln");
        assert!(cache.get("https://example.test/x").is_none());
    }

    #[test]
    fn writing_leaves_no_temporary_files() {
        let root = scratch("clean");
        let cache = MetadataCache::new(&root);
        cache.put("https://example.test/x", "body").unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        cache.clear().unwrap();
    }

    #[test]
    fn clearing_a_cache_that_was_never_created_succeeds() {
        assert!(MetadataCache::new(scratch("absent")).clear().is_ok());
    }
}
