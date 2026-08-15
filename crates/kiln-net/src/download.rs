//! Fetching an artifact and proving it is the one that was asked for.
//!
//! This is the security-critical path in Kiln. Everything it downloads will be
//! put on a developer's `PATH` and executed, so the rule is absolute: bytes are
//! hashed as they arrive, compared against the digest the caller demanded, and a
//! mismatch destroys the file rather than reporting a warning.
//!
//! The digest — not the URL — is what makes an artifact acceptable. A mirror is
//! fine. A different payload from the official host is not.
//!
//! # Why the transfer runs on its own thread
//!
//! Kiln enforces its own stall timeout rather than relying on the HTTP client's,
//! because the client's does not do what is needed here.
//!
//! `ureq` offers `timeout_recv_body`, which sounds right and is not: it is a
//! deadline for receiving the *whole* body, so any value tight enough to catch a
//! dead connection also kills a healthy download of a large runtime on a slow
//! link. Worse, it was observed not to apply at all over TLS — `TransportAdapter`
//! starts life with a `NotHappening` timeout, and the socket read timeout is only
//! set when a finite one is computed, so a stalled HTTPS connection blocks in
//! `recvfrom` indefinitely. A real `kiln install` sat for twenty minutes at
//! 3.8 KB/s against a 120-second limit, with a progress bar that never moved.
//!
//! What is actually wanted is "no bytes at all for a while", which is the one
//! shape that distinguishes a dead transfer from a slow one. A blocking read
//! cannot be timed out from the thread performing it, so the transfer runs on a
//! worker and the calling thread watches a byte counter. Slow-but-alive is
//! allowed to take as long as it likes, exactly as documented.
//!
//! A stalled worker is abandoned rather than joined — it is stuck in a kernel
//! read that nothing here can interrupt. That is bounded (at most
//! three per download, all of which end when the process does), and
//! each attempt writes to its own uniquely named part file, so an abandoned
//! worker waking up later cannot scribble on a retry's download.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

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
const RETRY_BACKOFF: Duration = Duration::from_secs(1);

/// How long a transfer may go without a single byte arriving.
///
/// Generous on purpose. This is not a throughput floor — a download creeping
/// along at a few kilobytes a second is slow, not broken, and Kiln lets it
/// finish. It only has to be short enough that a connection which has genuinely
/// died is reported while someone is still watching.
#[cfg(not(test))]
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// The same, shortened so the stall tests take seconds rather than minutes.
///
/// What the tests pin down is that a stall is *detected and reported*; sixty
/// seconds is a tuning choice, not the behaviour.
#[cfg(test)]
const STALL_TIMEOUT: Duration = Duration::from_millis(1500);

/// How often the calling thread looks at the byte counter.
///
/// Also the tick rate for the progress bar, so it stays smooth without the
/// worker having to touch the terminal.
const STALL_POLL: Duration = Duration::from_millis(250);

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
            match self.fetch(http, destination, progress, attempt) {
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

    /// Run one attempt on a worker, watching it for a stall from here.
    fn fetch(
        &self,
        http: &Http,
        destination: &Path,
        progress: &mut dyn Progress,
        attempt: u32,
    ) -> Result<u64> {
        http.check_online(self.what)?;

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .io_context("Could not create the download directory", parent)?;
        }

        // Its own file per attempt, so a worker abandoned for stalling cannot
        // write into the file a later attempt is building.
        let part = part_path(destination, attempt);
        let counter = Arc::new(AtomicU64::new(0));
        let (events, incoming) = mpsc::channel();

        {
            // Everything the worker touches is owned by it: the calling thread
            // may walk away at any moment.
            let http = http.clone();
            let url = self.url.to_string();
            let what = self.what.to_string();
            let expected = self.expected.clone();
            let declared_size = self.size;
            let (part, destination) = (part.clone(), destination.to_path_buf());
            let counter = Arc::clone(&counter);

            std::thread::Builder::new()
                .name("kiln-download".into())
                .spawn(move || {
                    let outcome = transfer(
                        &http,
                        &url,
                        &what,
                        &expected,
                        declared_size,
                        &part,
                        &destination,
                        &counter,
                        &events,
                    );
                    // The receiver is gone if this attempt was abandoned, which
                    // is not an error — it is the whole point.
                    let _ = events.send(Event::Done(outcome));
                })
                .map_err(|e| Error::internal("Could not start the download").with_source(e))?;
        }

        let mut reported = 0u64;
        let mut last_seen = 0u64;
        let mut last_change = Instant::now();

        loop {
            match incoming.recv_timeout(STALL_POLL) {
                Ok(Event::Started(declared)) => progress.start(declared),
                Ok(Event::Done(outcome)) => {
                    // A last advance so the bar reaches the end it reported.
                    let seen = counter.load(Ordering::Relaxed);
                    progress.advance(seen.saturating_sub(reported));
                    progress.finish();
                    return outcome;
                }
                Err(RecvTimeoutError::Timeout) => {
                    let seen = counter.load(Ordering::Relaxed);
                    if seen != last_seen {
                        last_seen = seen;
                        last_change = Instant::now();
                    } else if last_change.elapsed() >= STALL_TIMEOUT {
                        progress.finish();
                        return Err(stalled(self.what, self.url, seen));
                    }
                    progress.advance(seen.saturating_sub(reported));
                    reported = seen;
                }
                // The worker died without reporting, which it is written not to
                // do. Treated as a transport failure so the retry still applies.
                Err(RecvTimeoutError::Disconnected) => {
                    progress.finish();
                    return Err(Error::new(
                        ErrorKind::Network,
                        format!("The download of {} ended unexpectedly", self.what),
                    )
                    .because(format!(
                        "{}: the transfer stopped without a result.",
                        self.url
                    ))
                    .hint("run the command again"));
                }
            }
        }
    }
}

/// What a worker tells the thread watching it.
enum Event {
    /// Headers are in; here is the size the server declared, if any.
    Started(Option<u64>),
    /// The transfer finished, one way or the other.
    Done(Result<u64>),
}

/// Where one attempt writes before it is promoted to `destination`.
fn part_path(destination: &Path, attempt: u32) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".part{attempt}"));
    destination.with_file_name(name)
}

/// The transfer itself. Runs on a worker thread and touches no terminal.
#[allow(clippy::too_many_arguments)]
fn transfer(
    http: &Http,
    url: &str,
    what: &str,
    expected: &Digest,
    declared_size: Option<u64>,
    part: &Path,
    destination: &Path,
    counter: &AtomicU64,
    events: &mpsc::Sender<Event>,
) -> Result<u64> {
    let mut response = http
        .download_agent()
        .get(url)
        .call()
        .map_err(|e| transport_error(e, url, what))?;

    check_status(response.status().as_u16(), url, what)?;

    let declared = response.body().content_length().or(declared_size);
    if let Some(declared) = declared
        && declared > MAX_ARTIFACT_BYTES
    {
        return Err(oversize(what, url, declared));
    }
    let _ = events.send(Event::Started(declared));

    let mut reader = response.body_mut().as_reader();
    let mut file =
        std::fs::File::create(part).io_context("Could not create the download file", part)?;

    let mut hasher = Digest::hasher(expected.algorithm());
    let mut buffer = vec![0u8; CHUNK_BYTES];
    let mut written: u64 = 0;

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(e) => {
                return Err(Error::new(
                    ErrorKind::Network,
                    format!("The download of {what} was interrupted"),
                )
                .because(format!("{url}: {e}"))
                .hint("run the command again; Kiln does not reuse a partial download")
                .with_source(e));
            }
        };

        written += read as u64;
        if written > MAX_ARTIFACT_BYTES {
            return Err(oversize(what, url, written));
        }

        let chunk = &buffer[..read];
        hasher.update(chunk);
        file.write_all(chunk)
            .map_err(|e| Error::io("Could not write the download", part, e))?;

        // Published after the bytes are safely in the file, so the watching
        // thread never sees progress that has not happened.
        counter.store(written, Ordering::Relaxed);
    }

    file.flush()
        .io_context("Could not write the download", part)?;
    // Force the bytes out before anything is verified against them, so a
    // crash cannot leave a file that passed verification but is not on disk.
    file.sync_all()
        .io_context("Could not flush the download to disk", part)?;
    drop(file);

    let actual = hasher.finish();
    if actual != *expected {
        let _ = std::fs::remove_file(part);
        return Err(digest_mismatch(what, url, expected, &actual));
    }

    // Only a verified artifact gets the name the caller asked for.
    std::fs::rename(part, destination).io_context("Could not finish the download", destination)?;

    Ok(written)
}

/// A transfer that stopped receiving anything.
fn stalled(what: &str, url: &str, received: u64) -> Error {
    Error::new(
        ErrorKind::Network,
        format!("The download of {what} stopped responding"),
    )
    .because(format!(
        "{url} sent nothing for {} seconds, after {received} bytes. \
         The connection is still open, so this is a stalled server or network \
         rather than a refused one.",
        STALL_TIMEOUT.as_secs()
    ))
    .hint("run the command again; Kiln does not reuse a partial download")
    .hint("if it keeps happening at the same point, the mirror may be unhealthy")
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

    /// A local HTTP server, for exercising the transfer without the network.
    ///
    /// Plain HTTP and 127.0.0.1 only — these run in the default `cargo test`
    /// suite, which has to work on a plane.
    struct Server {
        port: u16,
    }

    impl Server {
        /// Serve `body` in full to every connection.
        fn serving(body: &'static [u8]) -> Self {
            Self::spawn(move |socket| {
                let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(body);
                let _ = socket.flush();
            })
        }

        /// Send headers and `prefix`, then go silent without ever closing.
        ///
        /// This is the failure that hung a real install: the connection stays
        /// established, so nothing at the socket level ever reports an error.
        fn stalling(prefix: &'static [u8], declared: usize) -> Self {
            Self::spawn(move |socket| {
                let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\n\r\n");
                let _ = socket.write_all(header.as_bytes());
                let _ = socket.write_all(prefix);
                let _ = socket.flush();
                std::thread::sleep(Duration::from_secs(120));
            })
        }

        fn spawn(handle: impl Fn(&mut std::net::TcpStream) + Send + 'static) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();

            std::thread::spawn(move || {
                // Every retry opens a new connection, so keep accepting.
                for socket in listener.incoming() {
                    let Ok(mut socket) = socket else { break };
                    let mut scratch = [0u8; 2048];
                    let _ = socket.read(&mut scratch);
                    handle(&mut socket);
                }
            });
            Server { port }
        }

        fn url(&self) -> String {
            format!("http://127.0.0.1:{}/artifact.tar.gz", self.port)
        }
    }

    #[test]
    fn a_complete_download_is_verified_and_promoted() {
        let body: &[u8] = b"node-22.14.0-payload";
        let server = Server::serving(body);
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("artifact.tar.gz");
        let expected = digest_of(body);

        let written = Download {
            url: &server.url(),
            what: "Node.js 22.14.0",
            expected: &expected,
            size: None,
        }
        .to_file(&Http::new(false), &destination, &mut SilentProgress)
        .expect("a healthy transfer");

        assert_eq!(written, body.len() as u64);
        assert_eq!(std::fs::read(&destination).unwrap(), body);
        // The part file is a staging detail and must not survive.
        assert!(!part_path(&destination, 1).exists());
    }

    #[test]
    fn a_download_whose_bytes_are_wrong_is_destroyed() {
        let server = Server::serving(b"something else entirely");
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("artifact.tar.gz");
        let expected = digest_of(b"what was asked for");

        let error = Download {
            url: &server.url(),
            what: "Node.js 22.14.0",
            expected: &expected,
            size: None,
        }
        .to_file(&Http::new(false), &destination, &mut SilentProgress)
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Verification);
        assert!(!destination.exists(), "a wrong artifact must not survive");
        assert!(!part_path(&destination, 1).exists());
    }

    #[test]
    fn a_stalled_transfer_is_reported_instead_of_hanging() {
        // The regression this exists for. Before Kiln watched the byte counter
        // itself, this blocked in `recvfrom` for as long as the server cared to
        // hold the connection open — twenty minutes, in the case that found it.
        let server = Server::stalling(b"the first few bytes", 10_000_000);
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("artifact.tar.gz");
        let expected = digest_of(b"never arrives");

        let started = Instant::now();
        let error = Download {
            url: &server.url(),
            what: "Node.js 22.14.0",
            expected: &expected,
            size: None,
        }
        .to_file(&Http::new(false), &destination, &mut SilentProgress)
        .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::Network);
        assert!(
            error.summary().contains("stopped responding"),
            "got: {}",
            error.summary()
        );
        // Three attempts plus backoff, each bounded by the stall timeout.
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "took {:?} — the stall watchdog did not fire",
            started.elapsed()
        );
        assert!(!destination.exists());
    }

    #[test]
    fn a_stall_says_how_much_arrived_before_it_died() {
        let server = Server::stalling(b"0123456789", 10_000_000);
        let error = stalled("Node.js 22.14.0", &server.url(), 10);

        // "nothing at all" and "died three quarters of the way through" are very
        // different situations to be told about.
        assert!(error.reason().unwrap().contains("after 10 bytes"));
        assert!(error.reason().unwrap().contains("stalled server"));
    }

    #[test]
    fn each_attempt_writes_to_its_own_part_file() {
        // An abandoned worker may still be writing when the next attempt
        // starts. Sharing one path would let it corrupt the retry.
        let destination = Path::new("/staging/artifact.tar.gz");
        let names: Vec<String> = (1..=MAX_ATTEMPTS)
            .map(|n| {
                part_path(destination, n)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(
            names,
            [
                "artifact.tar.gz.part1",
                "artifact.tar.gz.part2",
                "artifact.tar.gz.part3"
            ]
        );
        assert_eq!(
            part_path(destination, 1).parent(),
            Some(Path::new("/staging")),
            "the part file must stay beside its destination, on one filesystem"
        );
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
