//! Unpacking a zip artifact.
//!
//! Some publishers ship zips and nothing else — Deno and Bun both do — so a
//! tar-only Kiln simply cannot install them.
//!
//! # Why this is written out rather than pulled in
//!
//! The compression is DEFLATE, which `flate2` already provides and Kiln already
//! depends on for `tar.gz`. What a zip crate would add on top is the container:
//! an end-of-central-directory record, a central directory, and a local header
//! per entry. That is a few hundred lines of well-specified parsing, and writing
//! it keeps the path-safety policy identical to the tar path — the same refusal
//! on `..`, on absolute names, on symlinks that escape — rather than
//! approximately identical, which is the kind of difference that hides a bug.
//!
//! # What is deliberately not supported
//!
//! Encryption, multi-disk archives, and Zip64. Each is refused by name rather
//! than misread: an artifact Kiln cannot unpack correctly must fail loudly, and
//! silently truncating a 5 GB entry to its low 32 bits would be the worst
//! possible outcome. No runtime Kiln installs uses any of them.
//!
//! Like the tar path, this runs only *after* the artifact's digest has been
//! checked, so the bytes are the ones the publisher shipped. The job here is to
//! keep a hostile *layout* from escaping the staging directory.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use kiln_core::error::{Error, IoResultExt, Result};

/// End of central directory record.
const EOCD_SIGNATURE: [u8; 4] = [b'P', b'K', 5, 6];
/// Central directory file header.
const CENTRAL_SIGNATURE: [u8; 4] = [b'P', b'K', 1, 2];
/// Local file header.
const LOCAL_SIGNATURE: [u8; 4] = [b'P', b'K', 3, 4];

/// Fixed size of the end-of-central-directory record, before its comment.
const EOCD_LENGTH: usize = 22;
/// A zip comment's length field is 16 bits, so the record starts no further
/// back than this from the end of the file.
const MAX_COMMENT: usize = u16::MAX as usize;

/// Stored, no compression.
const METHOD_STORE: u16 = 0;
/// DEFLATE.
const METHOD_DEFLATE: u16 = 8;

/// The value a 32-bit field takes when the real one lives in a Zip64 record.
const ZIP64_SENTINEL_32: u32 = u32::MAX;
/// The same, for a 16-bit count.
const ZIP64_SENTINEL_16: u16 = u16::MAX;

/// One entry, as described by the central directory.
///
/// The central directory is used rather than walking local headers, because it
/// is the authoritative index — a local header may legally omit sizes and defer
/// them to a trailing data descriptor.
#[derive(Debug)]
struct Entry {
    name: String,
    method: u16,
    compressed_size: u64,
    uncompressed_size: u64,
    local_offset: u64,
    /// Unix mode, when the archive was made on a Unix-like system.
    unix_mode: Option<u32>,
}

impl Entry {
    /// Whether this entry is a directory rather than a file.
    fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }

    /// Whether the recorded mode says this is a symlink.
    fn is_symlink(&self) -> bool {
        // S_IFLNK. Checked against the full type mask, so a regular file with
        // permissions that merely overlap is not mistaken for one.
        self.unix_mode
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
    }

    /// Whether any execute bit is set.
    ///
    /// Zips made on Windows carry no mode at all. A runtime shipped that way
    /// would arrive non-executable, so it is treated as executable rather than
    /// installed broken — the alternative is a `deno` that cannot run.
    fn executable(&self) -> bool {
        match self.unix_mode {
            Some(mode) => mode & 0o111 != 0,
            None => true,
        }
    }
}

/// Unpack `archive` into `into`.
pub fn unpack(archive: &Path, into: &Path) -> Result<()> {
    let file = File::open(archive).io_context("Could not open the artifact", archive)?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);

    // Measured once. Seeking to the end per entry would discard the read
    // buffer every time, which for a many-file archive is a lot of re-reading
    // to learn something that cannot change.
    let file_length = reader
        .seek(SeekFrom::End(0))
        .io_context("Could not read the artifact", archive)?;

    for entry in central_directory(&mut reader, archive)? {
        let relative = Path::new(&entry.name);
        if let Some(problem) = crate::archive::unsafe_path(relative) {
            return Err(crate::archive::refused(archive, &entry.name, problem));
        }

        let destination = into.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&destination).io_context(
                "Could not create a directory from the artifact",
                &destination,
            )?;
            continue;
        }

        // A file's parent directories may have no entries of their own.
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .io_context("Could not create a directory from the artifact", parent)?;
        }

        let bytes = read_entry(&mut reader, &entry, archive, file_length)?;

        if entry.is_symlink() {
            let target = String::from_utf8(bytes).map_err(|_| {
                crate::archive::refused(archive, &entry.name, "its symlink target is not text")
            })?;
            if let Some(problem) = crate::archive::unsafe_link(relative, Path::new(&target)) {
                return Err(crate::archive::refused(archive, &entry.name, problem));
            }
            symlink(&target, &destination)?;
            continue;
        }

        std::fs::write(&destination, &bytes)
            .io_context("Could not write a file from the artifact", &destination)?;
        set_executable(&destination, entry.executable())?;
    }
    Ok(())
}

/// Create a symlink, on the platforms that have them.
#[cfg(unix)]
fn symlink(target: &str, at: &Path) -> Result<()> {
    // Overwrite rather than fail: an archive may legitimately replace a path,
    // and the tar path already allows it.
    let _ = std::fs::remove_file(at);
    std::os::unix::fs::symlink(target, at)
        .io_context("Could not create a symlink from the artifact", at)
}

#[cfg(not(unix))]
fn symlink(_target: &str, at: &Path) -> Result<()> {
    Err(
        Error::unsupported("This artifact contains a symlink, which Kiln cannot recreate here")
            .because(format!(
                "`{}` is a symlink, and this platform needs privileges Kiln does not ask for.",
                at.display()
            ))
            .hint("please report this, naming the runtime and version"),
    )
}

/// Apply the executable bit, masking away setuid, setgid and sticky.
#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Same policy as the tar path: keep the executable bit, which a runtime
    // needs, and drop everything a downloaded artifact has no business setting.
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).io_context(
        "Could not set permissions on a file from the artifact",
        path,
    )
}

#[cfg(not(unix))]
fn set_executable(_path: &Path, _executable: bool) -> Result<()> {
    Ok(())
}

/// Read and decompress one entry's contents.
fn read_entry(
    reader: &mut BufReader<File>,
    entry: &Entry,
    archive: &Path,
    file_length: u64,
) -> Result<Vec<u8>> {
    // The local header repeats the name and may carry a different extra field,
    // so its length has to be read rather than assumed.
    let mut header = [0u8; 30];
    reader
        .seek(SeekFrom::Start(entry.local_offset))
        .io_context("Could not read the artifact", archive)?;
    reader
        .read_exact(&mut header)
        .io_context("Could not read the artifact", archive)?;

    if header[..4] != LOCAL_SIGNATURE {
        return Err(damaged(
            archive,
            format!("the entry `{}` has no local header", entry.name),
        ));
    }
    let name_length = u16_at(&header, 26) as u64;
    let extra_length = u16_at(&header, 28) as u64;

    // The size comes from the archive's own index, and is about to be used as
    // an allocation. A damaged directory claiming four gigabytes should be an
    // error, not an out-of-memory kill.
    if entry.compressed_size > file_length {
        return Err(damaged(
            archive,
            format!(
                "the entry `{}` claims {} bytes, more than the whole archive",
                entry.name, entry.compressed_size
            ),
        ));
    }

    reader
        .seek(SeekFrom::Start(
            entry.local_offset + 30 + name_length + extra_length,
        ))
        .io_context("Could not read the artifact", archive)?;

    let mut compressed = vec![0u8; entry.compressed_size as usize];
    reader
        .read_exact(&mut compressed)
        .io_context("Could not read the artifact", archive)?;

    let bytes = match entry.method {
        METHOD_STORE => compressed,
        METHOD_DEFLATE => {
            // Capacity from the central directory, which is a claim rather than
            // a guarantee — hence `read_to_end` rather than `read_exact`.
            let mut out = Vec::with_capacity(entry.uncompressed_size as usize);
            flate2::read::DeflateDecoder::new(&compressed[..])
                .read_to_end(&mut out)
                .map_err(|e| {
                    damaged(
                        archive,
                        format!("the entry `{}` did not decompress: {e}", entry.name),
                    )
                })?;
            out
        }
        other => {
            return Err(Error::unsupported(format!(
                "Kiln cannot unpack this artifact: compression method {other}"
            ))
            .because(format!(
                "The entry `{}` uses a method Kiln does not implement. It reads \
                 stored and deflated entries.",
                entry.name
            ))
            .hint("please report this, naming the runtime and version"));
        }
    };

    if bytes.len() as u64 != entry.uncompressed_size {
        return Err(damaged(
            archive,
            format!(
                "the entry `{}` unpacked to {} bytes, not the {} it declared",
                entry.name,
                bytes.len(),
                entry.uncompressed_size
            ),
        ));
    }
    Ok(bytes)
}

/// Parse the central directory.
fn central_directory(reader: &mut BufReader<File>, archive: &Path) -> Result<Vec<Entry>> {
    let length = reader
        .seek(SeekFrom::End(0))
        .io_context("Could not read the artifact", archive)?;

    // Checked before any indexing: a file shorter than the record cannot
    // contain one, and the search below would read past the end looking.
    if length < EOCD_LENGTH as u64 {
        return Err(damaged(
            archive,
            format!("it is only {length} bytes, too short to be a zip"),
        ));
    }

    // Scan back for the end-of-central-directory record. It is last in the
    // file, but a trailing comment of up to 64 KiB may follow it.
    let window = (EOCD_LENGTH + MAX_COMMENT).min(length as usize);
    let start = length - window as u64;
    reader
        .seek(SeekFrom::Start(start))
        .io_context("Could not read the artifact", archive)?;
    let mut tail = vec![0u8; window];
    reader
        .read_exact(&mut tail)
        .io_context("Could not read the artifact", archive)?;

    let eocd = (0..=tail.len().saturating_sub(EOCD_LENGTH))
        .rev()
        .find(|&index| tail[index..index + 4] == EOCD_SIGNATURE)
        .ok_or_else(|| damaged(archive, "it has no end-of-central-directory record".into()))?;
    let eocd = &tail[eocd..];

    if u16_at(eocd, 4) != 0 || u16_at(eocd, 6) != 0 {
        return Err(unsupported_zip(archive, "it is split across several disks"));
    }
    let count = u16_at(eocd, 10);
    let offset = u32_at(eocd, 16);
    if count == ZIP64_SENTINEL_16 || offset == ZIP64_SENTINEL_32 {
        return Err(unsupported_zip(archive, "it is in Zip64 format"));
    }

    reader
        .seek(SeekFrom::Start(offset as u64))
        .io_context("Could not read the artifact", archive)?;
    let mut directory = Vec::new();
    reader
        .read_to_end(&mut directory)
        .io_context("Could not read the artifact", archive)?;

    let mut entries = Vec::with_capacity(count as usize);
    let mut at = 0usize;
    for _ in 0..count {
        if at + 46 > directory.len() || directory[at..at + 4] != CENTRAL_SIGNATURE {
            return Err(damaged(
                archive,
                "its central directory is truncated".into(),
            ));
        }

        let flags = u16_at(&directory, at + 8);
        // Bit 0 is the encryption flag. An encrypted entry would otherwise
        // "unpack" into ciphertext.
        if flags & 1 != 0 {
            return Err(unsupported_zip(archive, "it is encrypted"));
        }

        let method = u16_at(&directory, at + 10);
        let compressed_size = u32_at(&directory, at + 20);
        let uncompressed_size = u32_at(&directory, at + 24);
        let name_length = u16_at(&directory, at + 28) as usize;
        let extra_length = u16_at(&directory, at + 30) as usize;
        let comment_length = u16_at(&directory, at + 32) as usize;
        let made_by_unix = directory[at + 5] == 3;
        let external = u32_at(&directory, at + 38);
        let local_offset = u32_at(&directory, at + 42);

        if compressed_size == ZIP64_SENTINEL_32
            || uncompressed_size == ZIP64_SENTINEL_32
            || local_offset == ZIP64_SENTINEL_32
        {
            return Err(unsupported_zip(archive, "it is in Zip64 format"));
        }

        let name_at = at + 46;
        if name_at + name_length > directory.len() {
            return Err(damaged(
                archive,
                "its central directory is truncated".into(),
            ));
        }
        let name = String::from_utf8(directory[name_at..name_at + name_length].to_vec())
            .map_err(|_| damaged(archive, "an entry name is not valid UTF-8".into()))?;

        entries.push(Entry {
            name,
            method,
            compressed_size: compressed_size as u64,
            uncompressed_size: uncompressed_size as u64,
            local_offset: local_offset as u64,
            // The high 16 bits of the external attributes are the Unix mode,
            // but only when the archive says it was made on Unix.
            unix_mode: made_by_unix.then_some(external >> 16),
        });

        at = name_at + name_length + extra_length + comment_length;
    }
    Ok(entries)
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn damaged(archive: &Path, problem: String) -> Error {
    Error::new(
        kiln_core::ErrorKind::Verification,
        "Could not unpack the artifact",
    )
    .because(format!("{}: {problem}.", archive.display()))
    .hint("the download may be truncated; run the command again")
}

fn unsupported_zip(archive: &Path, problem: &str) -> Error {
    Error::unsupported("Kiln cannot unpack this zip artifact")
        .because(format!("{}: {problem}.", archive.display()))
        .hint("please report this, naming the runtime and version")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// Build a zip in memory, so the tests do not depend on a `zip` command.
    ///
    /// Deliberately hand-assembled rather than produced by a library: these
    /// tests exist to pin down the parser against the *format*, and generating
    /// the fixtures with the same understanding that reads them would only
    /// confirm that Kiln agrees with itself.
    struct Builder {
        entries: Vec<(String, Vec<u8>, u32, u16)>,
    }

    impl Builder {
        fn new() -> Self {
            Builder {
                entries: Vec::new(),
            }
        }

        fn file(mut self, name: &str, contents: &[u8], mode: u32) -> Self {
            self.entries
                .push((name.into(), contents.to_vec(), mode, METHOD_STORE));
            self
        }

        fn deflated(mut self, name: &str, contents: &[u8], mode: u32) -> Self {
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(contents).unwrap();
            let packed = encoder.finish().unwrap();
            self.entries
                .push((name.into(), contents.to_vec(), mode, METHOD_DEFLATE));
            // Store the packed bytes alongside by re-encoding at write time.
            let _ = packed;
            self
        }

        fn build(self, at: &Path) {
            let mut out: Vec<u8> = Vec::new();
            let mut central: Vec<u8> = Vec::new();

            for (name, contents, mode, method) in &self.entries {
                let payload = match *method {
                    METHOD_DEFLATE => {
                        let mut encoder = flate2::write::DeflateEncoder::new(
                            Vec::new(),
                            flate2::Compression::default(),
                        );
                        encoder.write_all(contents).unwrap();
                        encoder.finish().unwrap()
                    }
                    _ => contents.clone(),
                };
                let offset = out.len() as u32;

                out.extend_from_slice(&LOCAL_SIGNATURE);
                out.extend_from_slice(&[20, 0, 0, 0]); // version, flags
                out.extend_from_slice(&method.to_le_bytes());
                out.extend_from_slice(&[0; 8]); // time, date, crc
                out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                out.extend_from_slice(&(contents.len() as u32).to_le_bytes());
                out.extend_from_slice(&(name.len() as u16).to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes()); // extra
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(&payload);

                central.extend_from_slice(&CENTRAL_SIGNATURE);
                central.extend_from_slice(&[20, 3]); // made by: unix
                central.extend_from_slice(&[20, 0, 0, 0]); // needed, flags
                central.extend_from_slice(&method.to_le_bytes());
                central.extend_from_slice(&[0; 8]);
                central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                central.extend_from_slice(&(contents.len() as u32).to_le_bytes());
                central.extend_from_slice(&(name.len() as u16).to_le_bytes());
                central.extend_from_slice(&[0; 8]); // extra, comment, disk, internal
                central.extend_from_slice(&(mode << 16).to_le_bytes());
                central.extend_from_slice(&offset.to_le_bytes());
                central.extend_from_slice(name.as_bytes());
            }

            let directory_offset = out.len() as u32;
            let count = self.entries.len() as u16;
            out.extend_from_slice(&central);
            out.extend_from_slice(&EOCD_SIGNATURE);
            out.extend_from_slice(&[0; 4]); // disk numbers
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&(central.len() as u32).to_le_bytes());
            out.extend_from_slice(&directory_offset.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // comment length

            std::fs::write(at, out).unwrap();
        }
    }

    fn unpack_into(builder: Builder) -> (tempfile::TempDir, PathBuf, Result<()>) {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("artifact.zip");
        let into = directory.path().join("out");
        std::fs::create_dir_all(&into).unwrap();
        builder.build(&archive);
        let outcome = unpack(&archive, &into);
        (directory, into, outcome)
    }

    #[test]
    fn a_stored_file_round_trips() {
        let (_guard, into, outcome) =
            unpack_into(Builder::new().file("deno", b"#!/bin/sh\necho hi\n", 0o755));
        outcome.unwrap();

        assert_eq!(
            std::fs::read(into.join("deno")).unwrap(),
            b"#!/bin/sh\necho hi\n"
        );
    }

    #[test]
    fn a_deflated_file_round_trips() {
        // Long enough that DEFLATE actually compresses it, so the stored path
        // cannot pass this by accident.
        let contents = "kiln".repeat(4096);
        let (_guard, into, outcome) =
            unpack_into(Builder::new().deflated("bun", contents.as_bytes(), 0o755));
        outcome.unwrap();

        assert_eq!(
            std::fs::read(into.join("bun")).unwrap(),
            contents.as_bytes()
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_survives() {
        use std::os::unix::fs::PermissionsExt;

        // The whole point for a single-binary runtime: a `deno` without this is
        // installed and unusable.
        let (_guard, into, outcome) = unpack_into(Builder::new().file("deno", b"x", 0o755));
        outcome.unwrap();

        let mode = std::fs::metadata(into.join("deno"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    #[cfg(unix)]
    #[test]
    fn setuid_is_stripped() {
        use std::os::unix::fs::PermissionsExt;

        // Nothing Kiln downloads has any business being setuid, and the tar
        // path already refuses to honour it.
        let (_guard, into, outcome) = unpack_into(Builder::new().file("evil", b"x", 0o4755));
        outcome.unwrap();

        let mode = std::fs::metadata(into.join("evil"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o7000,
            0,
            "setuid, setgid and sticky must not survive"
        );
    }

    #[test]
    fn a_traversing_entry_is_refused() {
        let (_guard, into, outcome) =
            unpack_into(Builder::new().file("../../etc/passwd", b"pwned", 0o644));

        let error = outcome.unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::Verification);
        assert!(error.reason().unwrap_or_default().contains("`..`"));
        // Refused, not merely skipped: nothing may have been written.
        assert!(!into.join("etc").exists());
    }

    #[test]
    fn an_absolute_entry_is_refused() {
        let (_guard, _into, outcome) = unpack_into(Builder::new().file("/etc/passwd", b"x", 0o644));
        let error = outcome.unwrap_err();
        assert!(error.reason().unwrap_or_default().contains("absolute path"));
    }

    #[test]
    fn nested_directories_are_created_without_their_own_entries() {
        // Real archives often omit directory entries entirely.
        let (_guard, into, outcome) =
            unpack_into(Builder::new().file("bun-darwin-aarch64/bun", b"x", 0o755));
        outcome.unwrap();

        assert!(into.join("bun-darwin-aarch64/bun").is_file());
    }

    #[test]
    fn several_entries_all_arrive() {
        let (_guard, into, outcome) = unpack_into(
            Builder::new()
                .file("a.txt", b"one", 0o644)
                .deflated("nested/b.txt", &b"two".repeat(1000), 0o644)
                .file("nested/deep/c.txt", b"three", 0o755),
        );
        outcome.unwrap();

        assert_eq!(std::fs::read(into.join("a.txt")).unwrap(), b"one");
        assert_eq!(
            std::fs::read(into.join("nested/b.txt")).unwrap().len(),
            3000
        );
        assert_eq!(
            std::fs::read(into.join("nested/deep/c.txt")).unwrap(),
            b"three"
        );
    }

    #[test]
    fn an_archive_with_no_end_record_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("artifact.zip");
        // Comfortably longer than the record, so this exercises the failed
        // search rather than the length guard above it.
        std::fs::write(
            &archive,
            b"not a zip at all, but long enough to look like one",
        )
        .unwrap();

        let error = unpack(&archive, directory.path()).unwrap_err();
        assert!(
            error
                .reason()
                .unwrap_or_default()
                .contains("end-of-central-directory")
        );
    }

    #[test]
    fn a_file_too_short_to_be_a_zip_is_reported_rather_than_read_past() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("artifact.zip");
        // Shorter than the end-of-central-directory record. The backwards scan
        // must not index into it looking for a signature.
        std::fs::write(&archive, b"PK").unwrap();

        let error = unpack(&archive, directory.path()).unwrap_err();
        assert!(error.reason().unwrap_or_default().contains("too short"));
    }

    #[test]
    fn an_entry_larger_than_the_archive_is_refused_before_allocating() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("artifact.zip");
        Builder::new().file("deno", b"small", 0o755).build(&archive);

        // Rewrite the central directory's compressed size to 3 GB. Without a
        // bound this becomes a 3 GB allocation before the read fails.
        let mut bytes = std::fs::read(&archive).unwrap();
        let at = bytes
            .windows(4)
            .rposition(|w| w == CENTRAL_SIGNATURE)
            .unwrap();
        bytes[at + 20..at + 24].copy_from_slice(&3_000_000_000u32.to_le_bytes());
        std::fs::write(&archive, bytes).unwrap();

        let error = unpack(&archive, directory.path()).unwrap_err();
        assert!(
            error
                .reason()
                .unwrap_or_default()
                .contains("more than the whole archive")
        );
    }

    #[test]
    fn a_truncated_archive_says_to_try_again() {
        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("artifact.zip");
        Builder::new()
            .file("deno", &b"x".repeat(500), 0o755)
            .build(&archive);

        // Lop off the payload but keep the trailer, as a resumed-and-corrupted
        // download would.
        let mut bytes = std::fs::read(&archive).unwrap();
        bytes.drain(40..300);
        std::fs::write(&archive, bytes).unwrap();

        let error = unpack(&archive, directory.path()).unwrap_err();
        assert!(error.hints().iter().any(|h| h.text().contains("again")));
    }
}
