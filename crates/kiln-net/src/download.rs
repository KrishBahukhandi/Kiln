//! Fetching an artifact and proving it is the one that was asked for.
//!
//! This is the security-critical path in Kiln. Everything it downloads will be
//! put on a developer's `PATH` and executed, so the rule is absolute: bytes are
//! hashed as they arrive, compared against the digest the caller demanded, and a
//! mismatch destroys the file rather than reporting a warning.
//!
//! The digest — not the URL — is what makes an artifact acceptable. A mirror is
//! fine. A different payload from the official host is not.

use std::io::{Read, Write};
use std::path::Path;

use kiln_core::Digest;
use kiln_core::error::{Error, ErrorKind, IoResultExt, Result};

use crate::client::{Http, check_status, transport_error};

/// The largest artifact Kiln will accept.
///
/// Node.js and CPython distributions are tens of megabytes. This is a guard
/// against a hostile or broken server streaming until the disk fills, not a
/// considered limit on runtime size; raise it when a real runtime needs more.
pub const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

/// How much is read from the socket at a time.
const CHUNK_BYTES: usize = 64 * 1024;

/// How many times a download is attempted before giving up.
///
/// A dropped connection part-way through a fifty-megabyte transfer is a normal
/// event on real networks, not an error worth handing back to a person. Only
/// *transport* failures are retried: a digest mismatch is never retried, because
/// silently trying again is exactly the wrong response to bytes that were not
/// what they claimed to be.
const MAX_ATTEMPTS: u32 = 3;

/// How long to wait after the first failure. Doubles each time.
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(1);

/// Reports download progress to whatever is watching.
///
/// A trait rather than a concrete progress bar so this crate does not depend on
/// a terminal library, and so `--quiet`, non-TTY and JSON modes can supply
/// something that does nothing.
pub trait Progress {
    /// Called once, with the expected total when the server declares one.
    fn start(&mut self, total: Option<u64>);
    /// Called with each chunk's size as it arrives.
    fn advance(&mut self, bytes: u64);
    /// Called once when the transfer ends, successfully or not.
    fn finish(&mut self);
}

/// A [`Progress`] implementation that reports nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct SilentProgress;

impl Progress for SilentProgress {
    fn start(&mut self, _total: Option<u64>) {}
    fn advance(&mut self, _bytes: u64) {}
    fn finish(&mut self) {}
}

/// One artifact to fetch, and the digest it must have.
pub struct Download<'a> {
    /// Where to fetch it from. Advisory — the digest is what is trusted.
    pub url: &'a str,
    /// What this is, in the user's terms: `Node.js 22.14.0`.
    pub what: &'a str,
    /// The digest the downloaded bytes must have.
    pub expected: &'a Digest,
    /// The size the publisher stated, if any. Used only for progress.
    pub size: Option<u64>,
}

impl Download<'_> {
    /// Fetch to `destination`, verifying as the bytes arrive.
    ///
    /// Returns the number of bytes written. On any failure — transport,
    /// oversize, or digest mismatch — `destination` is removed, so a partial or
    /// wrong artifact never survives to be mistaken for a good one.
    pub fn to_file(
        &self,
        http: &Http,
        destination: &Path,
        progress: &mut dyn Progress,
    ) -> Result<u64> {
        let mut backoff = RETRY_BACKOFF;

        for attempt in 1..=MAX_ATTEMPTS {
            match self.fetch(http, destination, progress) {
                Ok(written) => return Ok(written),
                Err(error) => {
                    // Nothing partial survives a failed attempt, so a retry
                    // starts from a clean file rather than appending.
                    let _ = std::fs::remove_file(destination);

                    let retryable = error.kind() == ErrorKind::Network && attempt < MAX_ATTEMPTS;
                    if !retryable {
                        return Err(if attempt > 1 {
                            error.hint(format!("Kiln tried {attempt} times"))
                        } else {
                            error
                        });
                    }

                    tracing::debug!(
                        attempt,
                        url = self.url,
                        "download failed, retrying: {error}"
                    );
                    std::thread::sleep(backoff);
                    backoff *= 2;
                }
            }
        }

        // `MAX_ATTEMPTS` is non-zero, so the loop always returns.
        Err(Error::internal(
            "Download retry loop ended without a result",
        ))
    }

    fn fetch(&self, http: &Http, destination: &Path, progress: &mut dyn Progress) -> Result<u64> {
        http.check_online(self.what)?;

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .io_context("Could not create the download directory", parent)?;
        }

        let mut response = http
            .download_agent()
            .get(self.url)
            .call()
            .map_err(|e| transport_error(e, self.url, self.what))?;

        check_status(response.status().as_u16(), self.url, self.what)?;

        let declared = response.body().content_length().or(self.size);
        if let Some(declared) = declared
            && declared > MAX_ARTIFACT_BYTES
        {
            return Err(oversize(self.what, self.url, declared));
        }
        progress.start(declared);

        let mut reader = response.body_mut().as_reader();
        let mut file = std::fs::File::create(destination)
            .io_context("Could not create the download file", destination)?;

        let mut hasher = Digest::hasher(self.expected.algorithm());
        let mut buffer = vec![0u8; CHUNK_BYTES];
        let mut written: u64 = 0;

        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(e) => {
                    progress.finish();
                    return Err(Error::new(
                        ErrorKind::Network,
                        format!("The download of {} was interrupted", self.what),
                    )
                    .because(format!("{}: {e}", self.url))
                    .hint("run the command again; Kiln does not reuse a partial download")
                    .with_source(e));
                }
            };

            written += read as u64;
            if written > MAX_ARTIFACT_BYTES {
                progress.finish();
                return Err(oversize(self.what, self.url, written));
            }

            let chunk = &buffer[..read];
            hasher.update(chunk);
            if let Err(e) = file.write_all(chunk) {
                progress.finish();
                return Err(Error::io("Could not write the download", destination, e));
            }
            progress.advance(read as u64);
        }

        file.flush()
            .io_context("Could not write the download", destination)?;
        // Force the bytes out before anything is verified against them, so a
        // crash cannot leave a file that passed verification but is not on disk.
        file.sync_all()
            .io_context("Could not flush the download to disk", destination)?;
        drop(file);
        progress.finish();

        let actual = hasher.finish();
        if actual != *self.expected {
            return Err(digest_mismatch(self.what, self.url, self.expected, &actual));
        }

        Ok(written)
    }
}

/// Re-verify a file already on disk against a digest.
///
/// Used when a staged artifact is reused, and by `kiln cache verify`.
pub fn verify_file(path: &Path, expected: &Digest, what: &str) -> Result<()> {
    let actual = Digest::of_file(expected.algorithm(), path)?;
    if actual != *expected {
        return Err(digest_mismatch(
            what,
            &path.display().to_string(),
            expected,
            &actual,
        ));
    }
    Ok(())
}

fn digest_mismatch(what: &str, source: &str, expected: &Digest, actual: &Digest) -> Error {
    Error::new(
        ErrorKind::Verification,
        format!("{what} failed verification"),
    )
    .because(format!(
        "The bytes from {source} do not match the digest Kiln required.\n\
         Kiln has discarded them and installed nothing."
    ))
    .expected(format!("expected  {expected}\nreceived  {actual}"))
    .hint("a corrupted download is the usual cause; run the command again")
    .hint("if it repeats, the artifact or the mirror serving it may have been tampered with")
}

fn oversize(what: &str, url: &str, size: u64) -> Error {
    Error::new(
        ErrorKind::Verification,
        format!("{what} is larger than Kiln will accept"),
    )
    .because(format!(
        "{url} is serving at least {size} bytes, and Kiln's limit is {MAX_ARTIFACT_BYTES}."
    ))
    .hint("this usually means the URL is not the artifact it claims to be")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::HashAlgorithm;

    fn digest_of(data: &[u8]) -> Digest {
        Digest::of_bytes(HashAlgorithm::Sha256, data)
    }

    #[test]
    fn offline_downloads_fail_before_creating_anything() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("artifact.tar.gz");
        let expected = digest_of(b"whatever");

        let error = Download {
            url: "https://nodejs.org/dist/v22.14.0/node.tar.gz",
            what: "Node.js 22.14.0",
            expected: &expected,
            size: None,
        }
        .to_file(&Http::new(true), &destination, &mut SilentProgress)
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Network);
        assert!(!destination.exists(), "nothing should have been written");
    }

    #[test]
    fn verification_of_a_good_file_passes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("artifact");
        std::fs::write(&path, b"node-22.14.0").unwrap();

        assert!(verify_file(&path, &digest_of(b"node-22.14.0"), "Node.js 22.14.0").is_ok());
    }

    #[test]
    fn verification_of_a_tampered_file_fails_loudly() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("artifact");
        std::fs::write(&path, b"something else entirely").unwrap();

        let expected = digest_of(b"node-22.14.0");
        let error = verify_file(&path, &expected, "Node.js 22.14.0").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Verification);
        assert!(error.summary().contains("failed verification"));
        assert!(error.reason().unwrap().contains("installed nothing"));

        // Both digests are shown, so the mismatch can be investigated.
        let expectation = error.expectation().unwrap();
        assert!(expectation.contains(&expected.to_string()));
        assert!(expectation.contains("received"));
    }

    #[test]
    fn a_mismatch_suggests_both_innocent_and_hostile_explanations() {
        let error = digest_mismatch(
            "Node.js 22.14.0",
            "https://nodejs.org/x",
            &digest_of(b"a"),
            &digest_of(b"b"),
        );
        let hints: Vec<&str> = error.hints().iter().map(|h| h.text()).collect();
        assert!(hints.iter().any(|h| h.contains("corrupted")));
        assert!(hints.iter().any(|h| h.contains("tampered")));
    }

    #[test]
    fn the_size_guard_names_the_limit() {
        let error = oversize("Node.js 22.14.0", "https://example.test/x", u64::MAX);
        assert_eq!(error.kind(), ErrorKind::Verification);
        assert!(
            error
                .reason()
                .unwrap()
                .contains(&MAX_ARTIFACT_BYTES.to_string())
        );
    }

    #[test]
    fn verifying_a_missing_file_is_an_io_error_not_a_pass() {
        let error = verify_file(
            Path::new("/nonexistent/artifact"),
            &digest_of(b""),
            "Node.js 22.14.0",
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Io);
    }
}
