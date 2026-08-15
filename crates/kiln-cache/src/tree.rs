//! The canonical form of an unpacked tree.
//!
//! A store entry is named by the digest of the **archive** it came from, not by
//! a hash of the tree that archive unpacked into. That is the right name — it is
//! what the vendor publishes and attests to — but it means the entry's own name
//! cannot answer "are the bytes on disk still the bytes we unpacked?". Nothing
//! in a content-addressed store is self-verifying once it has been unpacked.
//!
//! So Kiln records the answer at install time. Immediately after extracting a
//! *verified* archive, it walks the result and writes a manifest of what it saw
//! into the entry beside `content/`. `kiln cache verify` walks the tree again
//! and compares. The manifest is the tree's birth certificate: it is trustworthy
//! precisely because it was taken the moment the archive's digest checked out.
//!
//! # What is recorded
//!
//! For every path under `content/`, sorted:
//!
//! ```text
//! kiln-tree 1
//! d include
//! f x 92876032 4f2c…  bin/node
//! f - 1204    9ab1…  lib/x.js
//! l ../lib/node_modules/npm/bin/npm-cli.js  bin/npm
//! ```
//!
//! - **Files** carry size, the sha256 of their contents, and one permission bit.
//! - **Symlinks** carry their target, unfollowed. A symlink that gets retargeted
//!   is a change to the tree even though no file's contents moved.
//! - **Directories** are recorded so that an empty one still counts. Losing an
//!   empty directory a runtime expects is a real failure and an easy one to miss.
//!
//! # What is deliberately not recorded
//!
//! - **Modification times and ownership.** Neither survives a `cp -a` between
//!   machines, a restored backup, or a different `tar` implementation, and
//!   neither affects whether the runtime works. Recording them would make
//!   verification fail loudly for reasons no user could act on, which teaches
//!   people to ignore it.
//! - **Full permission bits.** Only "is this executable by anybody" is kept.
//!   The rest is a function of the extracting process's umask, so the same
//!   archive legitimately unpacks to different modes on two machines. The
//!   executable bit is the one whose loss actually breaks a runtime.
//!
//! # What this does and does not catch
//!
//! It catches corruption: bit-rot, a truncated file, a half-finished delete,
//! an editor that saved into the store by accident, a partially restored backup.
//!
//! It is **not** tamper-proofing. The manifest lives in the same directory as
//! the tree it describes, so anyone who can rewrite the tree can rewrite the
//! manifest. Defending against that needs a signature from somewhere Kiln does
//! not control, which is a different feature — see `SECURITY.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use kiln_core::error::{Error, IoResultExt, Result};
use kiln_core::{Digest, HashAlgorithm};

/// Format marker.
///
/// A change to *what* is recorded bumps this. An entry carrying an older
/// manifest is then reported as unverifiable rather than as corrupt, because a
/// manifest Kiln cannot interpret says nothing about the tree either way.
pub const MANIFEST_VERSION: u32 = 1;

/// The first line of every manifest.
const HEADER: &str = "kiln-tree 1";

/// Version 1 hashes file contents with sha256, so lines carry bare hex.
const ALGORITHM: HashAlgorithm = HashAlgorithm::Sha256;

/// What Kiln recorded about one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// A directory. Recorded so an empty one is still part of the tree.
    Dir,
    /// A regular file.
    File {
        /// Whether any execute bit was set.
        executable: bool,
        /// Size in bytes. Redundant against the digest, but it makes a
        /// truncated file report as "truncated" instead of "contents differ".
        size: u64,
        /// sha256 of the contents.
        digest: Digest,
    },
    /// A symlink, with its target exactly as stored — never followed.
    Symlink {
        /// The link target.
        target: String,
    },
    /// Anything else: a fifo, socket or device node.
    ///
    /// No runtime archive should contain one, and extraction refuses the
    /// dangerous cases. Recording the path without detail means an unexpected
    /// one is still noticed rather than silently ignored.
    Other,
}

impl Record {
    /// The single-letter tag used in the manifest.
    fn tag(&self) -> char {
        match self {
            Record::Dir => 'd',
            Record::File { .. } => 'f',
            Record::Symlink { .. } => 'l',
            Record::Other => 'o',
        }
    }

    /// A short human description, for diagnostics.
    fn describe(&self) -> &'static str {
        match self {
            Record::Dir => "directory",
            Record::File { .. } => "file",
            Record::Symlink { .. } => "symlink",
            Record::Other => "special file",
        }
    }
}

/// Every path in an unpacked tree, in a form that renders identically for
/// identical trees.
///
/// A [`BTreeMap`] rather than a `Vec`, so ordering is a property of the type
/// instead of something every construction site has to remember.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeManifest {
    paths: BTreeMap<String, Record>,
}

impl TreeManifest {
    /// How many paths the manifest covers.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether the manifest covers nothing at all.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Total size of every regular file.
    pub fn total_bytes(&self) -> u64 {
        self.paths
            .values()
            .map(|record| match record {
                Record::File { size, .. } => *size,
                _ => 0,
            })
            .sum()
    }

    /// How many regular files the manifest covers.
    pub fn file_count(&self) -> usize {
        self.paths
            .values()
            .filter(|record| matches!(record, Record::File { .. }))
            .count()
    }

    /// Render to the canonical text form.
    pub fn render(&self) -> String {
        // Manifests run to a few hundred kilobytes for a large runtime; sizing
        // the buffer up front avoids a few dozen reallocations.
        let mut out = String::with_capacity(64 + self.paths.len() * 96);
        out.push_str(HEADER);
        out.push('\n');

        for (path, record) in &self.paths {
            out.push(record.tag());
            match record {
                Record::Dir | Record::Other => {}
                Record::File {
                    executable,
                    size,
                    digest,
                } => {
                    let _ = write!(
                        out,
                        " {} {size} {}",
                        if *executable { 'x' } else { '-' },
                        digest.hex()
                    );
                }
                Record::Symlink { target } => {
                    let _ = write!(out, " {}", escape(target));
                }
            }
            out.push(' ');
            out.push_str(&escape(path));
            out.push('\n');
        }
        out
    }

    /// The digest of the rendered manifest — one value standing for the whole
    /// tree.
    pub fn digest(&self) -> Digest {
        Digest::of_bytes(ALGORITHM, self.render().as_bytes())
    }

    /// Parse the canonical text form back.
    pub fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();
        match lines.next() {
            Some(HEADER) => {}
            Some(other) => {
                return Err(Error::new(
                    kiln_core::ErrorKind::Verification,
                    "This entry's file manifest is in a format Kiln does not recognise",
                )
                .because(format!("It starts with `{}`.", other.trim()))
                .expected(format!("`{HEADER}`"))
                .hint("reinstall the runtime to record a fresh manifest"));
            }
            None => {
                return Err(Error::new(
                    kiln_core::ErrorKind::Verification,
                    "This entry's file manifest is empty",
                )
                .hint("reinstall the runtime to record a fresh manifest"));
            }
        }

        let mut paths = BTreeMap::new();
        for (index, line) in lines.enumerate() {
            if line.is_empty() {
                continue;
            }
            let (path, record) = parse_line(line).ok_or_else(|| {
                // Line 1 is the header, and `enumerate` starts at zero.
                let number = index + 2;
                Error::new(
                    kiln_core::ErrorKind::Verification,
                    "This entry's file manifest is damaged",
                )
                .because(format!("Line {number} could not be read: `{line}`"))
                .hint("reinstall the runtime to record a fresh manifest")
            })?;
            paths.insert(path, record);
        }
        Ok(TreeManifest { paths })
    }

    /// Compare a recorded manifest against what is on disk now.
    ///
    /// Ordered by path, so the report reads like a tree rather than like a hash
    /// map's iteration order.
    pub fn compare(&self, actual: &TreeManifest) -> Vec<Difference> {
        let mut differences = Vec::new();

        for (path, recorded) in &self.paths {
            match actual.paths.get(path) {
                None => differences.push(Difference {
                    path: path.clone(),
                    kind: DifferenceKind::Missing,
                    detail: format!("the {} is gone", recorded.describe()),
                }),
                Some(found) if found != recorded => {
                    differences.push(difference_between(path, recorded, found));
                }
                Some(_) => {}
            }
        }

        for path in actual.paths.keys() {
            if !self.paths.contains_key(path) {
                differences.push(Difference {
                    path: path.clone(),
                    kind: DifferenceKind::Unexpected,
                    detail: "this was not in the archive".into(),
                });
            }
        }

        differences.sort_by(|a, b| a.path.cmp(&b.path));
        differences
    }
}

/// Why one path failed to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifferenceKind {
    /// Recorded, but no longer on disk.
    Missing,
    /// On disk, but never recorded.
    Unexpected,
    /// Same size, different bytes — the signature of silent corruption.
    ContentChanged,
    /// A different number of bytes.
    SizeChanged,
    /// The executable bit was gained or lost.
    ModeChanged,
    /// A file became a directory, a symlink became a file, and so on.
    TypeChanged,
    /// A symlink now points somewhere else.
    TargetChanged,
}

impl DifferenceKind {
    /// A stable machine-readable name, for `--json`.
    pub fn as_str(self) -> &'static str {
        match self {
            DifferenceKind::Missing => "missing",
            DifferenceKind::Unexpected => "unexpected",
            DifferenceKind::ContentChanged => "content_changed",
            DifferenceKind::SizeChanged => "size_changed",
            DifferenceKind::ModeChanged => "mode_changed",
            DifferenceKind::TypeChanged => "type_changed",
            DifferenceKind::TargetChanged => "target_changed",
        }
    }
}

/// One path that no longer matches what was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Difference {
    /// The path, relative to `content/`.
    pub path: String,
    /// What kind of change it is.
    pub kind: DifferenceKind,
    /// A human-readable explanation.
    pub detail: String,
}

fn difference_between(path: &str, recorded: &Record, found: &Record) -> Difference {
    let make = |kind, detail: String| Difference {
        path: path.to_string(),
        kind,
        detail,
    };

    match (recorded, found) {
        (
            Record::File {
                executable: was_exec,
                size: was_size,
                digest: was_digest,
            },
            Record::File {
                executable: now_exec,
                size: now_size,
                digest: now_digest,
            },
        ) => {
            // Size first: "truncated to 12 bytes" is a far more useful thing to
            // read than "contents differ", and it usually names the cause.
            if was_size != now_size {
                make(
                    DifferenceKind::SizeChanged,
                    format!("was {was_size} bytes, is now {now_size}"),
                )
            } else if was_digest != now_digest {
                make(
                    DifferenceKind::ContentChanged,
                    "same size, different contents".into(),
                )
            } else {
                make(
                    DifferenceKind::ModeChanged,
                    if *was_exec && !*now_exec {
                        "is no longer executable".into()
                    } else {
                        "is now executable".into()
                    },
                )
            }
        }
        (Record::Symlink { target: was }, Record::Symlink { target: now }) => make(
            DifferenceKind::TargetChanged,
            format!("pointed at `{was}`, now points at `{now}`"),
        ),
        (recorded, found) => make(
            DifferenceKind::TypeChanged,
            format!(
                "was a {}, is now a {}",
                recorded.describe(),
                found.describe()
            ),
        ),
    }
}

/// Walk an unpacked tree and record what is there.
///
/// Every regular file is read in full, because a digest is the only thing that
/// distinguishes a corrupted file from an intact one of the same size. On a
/// 150 MB runtime that is a second or so of disk, which is the right price for
/// a command whose entire job is to be sure.
pub fn manifest_of(root: &Path) -> Result<TreeManifest> {
    let mut paths = BTreeMap::new();
    walk(root, root, &mut paths)?;
    Ok(TreeManifest { paths })
}

fn walk(root: &Path, directory: &Path, into: &mut BTreeMap<String, Record>) -> Result<()> {
    let mut children: Vec<PathBuf> = std::fs::read_dir(directory)
        .io_context("Could not read the installed runtime", directory)?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .collect();
    children.sort();

    for child in children {
        let metadata = std::fs::symlink_metadata(&child)
            .io_context("Could not inspect the installed runtime", &child)?;
        let relative = relative_path(root, &child)?;

        // Symlinks first: `is_dir` follows links, so a symlink to a directory
        // would otherwise be walked into and recorded as the directory itself.
        if metadata.is_symlink() {
            let target = std::fs::read_link(&child)
                .io_context("Could not read a symlink in the installed runtime", &child)?;
            into.insert(
                relative,
                Record::Symlink {
                    target: path_to_text(&target, &child)?,
                },
            );
        } else if metadata.is_dir() {
            into.insert(relative, Record::Dir);
            walk(root, &child, into)?;
        } else if metadata.is_file() {
            into.insert(
                relative,
                Record::File {
                    executable: is_executable(&metadata),
                    size: metadata.len(),
                    digest: Digest::of_file(ALGORITHM, &child)?,
                },
            );
        } else {
            into.insert(relative, Record::Other);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    // Windows has no execute bit; the field is recorded as `-` and compares
    // equal against itself, which is the honest answer rather than a guess
    // derived from the file extension.
    false
}

/// A path below `root`, as `/`-separated text.
fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        Error::internal("A file in the store was found outside the tree being walked")
    })?;

    let mut text = String::new();
    for component in relative.components() {
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(&component_text(component, path)?);
    }
    Ok(text)
}

fn component_text(component: std::path::Component<'_>, path: &Path) -> Result<String> {
    component
        .as_os_str()
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| non_utf8(path))
}

fn path_to_text(path: &Path, context: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| non_utf8(context))
}

/// A path Kiln cannot render as text.
///
/// Refused rather than replaced with `U+FFFD`: a lossy conversion makes two
/// different paths render identically, which would silently weaken every
/// comparison the manifest exists to make.
fn non_utf8(path: &Path) -> Error {
    Error::new(
        kiln_core::ErrorKind::Unsupported,
        "This runtime contains a file name Kiln cannot record",
    )
    .because(format!(
        "`{}` is not valid UTF-8, so it cannot be written to a manifest without \
         losing the distinction between it and another name.",
        path.display()
    ))
    .hint("please report this, naming the runtime and version")
}

/// Escape a path or link target so it occupies exactly one whitespace-free
/// field.
///
/// Space and backslash are escaped so a name containing either cannot be
/// mistaken for a field boundary; control characters are escaped so a manifest
/// stays safe to print to a terminal. Everything else, including non-ASCII, is
/// left alone — it is unambiguous already and stays readable.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            c if c.is_ascii_graphic() => out.push(c),
            c if c.is_ascii() => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn unescape(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '\\' => out.push('\\'),
            'x' => {
                let high = chars.next()?;
                let low = chars.next()?;
                let mut hex = String::with_capacity(2);
                hex.push(high);
                hex.push(low);
                let byte = u8::from_str_radix(&hex, 16).ok()?;
                out.push(byte as char);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn parse_line(line: &str) -> Option<(String, Record)> {
    let (tag, rest) = line.split_at_checked(1)?;
    let rest = rest.strip_prefix(' ')?;

    match tag {
        "d" => Some((unescape(rest)?, Record::Dir)),
        "o" => Some((unescape(rest)?, Record::Other)),
        "l" => {
            let (target, path) = rest.split_once(' ')?;
            Some((
                unescape(path)?,
                Record::Symlink {
                    target: unescape(target)?,
                },
            ))
        }
        "f" => {
            let mut fields = rest.splitn(4, ' ');
            let executable = match fields.next()? {
                "x" => true,
                "-" => false,
                _ => return None,
            };
            let size = fields.next()?.parse().ok()?;
            let digest = Digest::parse(&format!("{ALGORITHM}:{}", fields.next()?)).ok()?;
            let path = unescape(fields.next()?)?;
            Some((
                path,
                Record::File {
                    executable,
                    size,
                    digest,
                },
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(executable: bool, contents: &[u8]) -> Record {
        Record::File {
            executable,
            size: contents.len() as u64,
            digest: Digest::of_bytes(ALGORITHM, contents),
        }
    }

    fn manifest(entries: &[(&str, Record)]) -> TreeManifest {
        TreeManifest {
            paths: entries
                .iter()
                .map(|(path, record)| ((*path).to_string(), record.clone()))
                .collect(),
        }
    }

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("content");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("bin/node"), b"#!/bin/sh\n").unwrap();
        std::fs::write(root.join("README.md"), b"hello").unwrap();
        (directory, root)
    }

    #[test]
    fn a_manifest_round_trips_through_its_text_form() {
        let original = manifest(&[
            ("bin", Record::Dir),
            ("bin/node", file(true, b"binary")),
            (
                "bin/npm",
                Record::Symlink {
                    target: "../lib/npm-cli.js".into(),
                },
            ),
            ("lib/x.js", file(false, b"module")),
            ("dev/null", Record::Other),
        ]);

        let parsed = TreeManifest::parse(&original.render()).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn rendering_is_stable_regardless_of_insertion_order() {
        let forwards = manifest(&[("a", Record::Dir), ("b", Record::Dir), ("c", Record::Dir)]);
        let backwards = manifest(&[("c", Record::Dir), ("b", Record::Dir), ("a", Record::Dir)]);

        // The digest stands in for the whole tree, so two identical trees have
        // to render byte-identically no matter how they were built.
        assert_eq!(forwards.render(), backwards.render());
        assert_eq!(forwards.digest(), backwards.digest());
    }

    #[test]
    fn a_name_with_a_space_survives_the_round_trip() {
        // Paths are the last field on a line, but a symlink target is not — so
        // an unescaped space would silently shift every field after it.
        let original = manifest(&[
            ("lib/my file.js", file(false, b"x")),
            (
                "bin/link",
                Record::Symlink {
                    target: "../my target".into(),
                },
            ),
        ]);

        let parsed = TreeManifest::parse(&original.render()).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn a_name_with_a_backslash_is_not_confused_with_an_escape() {
        let original = manifest(&[("lib/back\\slash", Record::Dir), ("lib/x20", Record::Dir)]);
        assert_eq!(TreeManifest::parse(&original.render()).unwrap(), original);
    }

    #[test]
    fn non_ascii_names_stay_readable() {
        let original = manifest(&[("lib/café/naïve.js", file(false, b"x"))]);
        assert!(
            original.render().contains("lib/café/naïve.js"),
            "a UTF-8 name is unambiguous already and should not be mangled"
        );
        assert_eq!(TreeManifest::parse(&original.render()).unwrap(), original);
    }

    #[test]
    fn a_manifest_from_another_format_version_is_refused() {
        let error = TreeManifest::parse("kiln-tree 2\nd bin\n").unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::Verification);
        assert!(error.summary().contains("does not recognise"));
    }

    #[test]
    fn a_damaged_line_names_its_line_number() {
        let error = TreeManifest::parse("kiln-tree 1\nd bin\nZ nonsense\n").unwrap_err();
        assert!(
            error.reason().unwrap_or_default().contains("Line 3"),
            "the report has to say where to look"
        );
    }

    #[test]
    fn walking_a_tree_records_every_path() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();

        assert_eq!(recorded.file_count(), 2);
        assert_eq!(recorded.total_bytes(), 15);
        // Directories count as paths, including the empty one.
        assert_eq!(recorded.len(), 4);
    }

    #[test]
    fn an_untouched_tree_verifies_against_itself() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();
        let again = manifest_of(&root).unwrap();
        assert!(recorded.compare(&again).is_empty());
    }

    #[test]
    fn corruption_that_preserves_size_is_still_caught() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();

        // The case a size check alone would miss, and the one bit-rot actually
        // looks like.
        std::fs::write(root.join("README.md"), b"HELLO").unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].path, "README.md");
        assert_eq!(differences[0].kind, DifferenceKind::ContentChanged);
    }

    #[test]
    fn a_truncated_file_is_reported_as_a_size_change() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();
        std::fs::write(root.join("bin/node"), b"").unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences[0].kind, DifferenceKind::SizeChanged);
        assert!(differences[0].detail.contains("is now 0"));
    }

    #[test]
    fn a_deleted_file_is_reported_as_missing() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();
        std::fs::remove_file(root.join("bin/node")).unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].kind, DifferenceKind::Missing);
    }

    #[test]
    fn a_lost_empty_directory_is_noticed() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();
        std::fs::remove_dir(root.join("empty")).unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].path, "empty");
    }

    #[test]
    fn a_file_added_to_the_store_is_reported() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();
        std::fs::write(root.join("bin/stowaway"), b"x").unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].kind, DifferenceKind::Unexpected);
    }

    #[cfg(unix)]
    #[test]
    fn losing_the_executable_bit_is_a_difference() {
        use std::os::unix::fs::PermissionsExt;

        let (_guard, root) = tree();
        std::fs::set_permissions(
            root.join("bin/node"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let recorded = manifest_of(&root).unwrap();

        // The failure mode this exists for: the bytes are perfect and the
        // runtime still will not start.
        std::fs::set_permissions(
            root.join("bin/node"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].kind, DifferenceKind::ModeChanged);
        assert!(differences[0].detail.contains("no longer executable"));
    }

    #[cfg(unix)]
    #[test]
    fn a_retargeted_symlink_is_a_difference() {
        let (_guard, root) = tree();
        std::os::unix::fs::symlink("bin/node", root.join("node")).unwrap();
        let recorded = manifest_of(&root).unwrap();

        std::fs::remove_file(root.join("node")).unwrap();
        std::os::unix::fs::symlink("README.md", root.join("node")).unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].kind, DifferenceKind::TargetChanged);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_recorded_rather_than_followed() {
        let (_guard, root) = tree();
        std::fs::create_dir(root.join("real")).unwrap();
        std::fs::write(root.join("real/file"), b"contents").unwrap();
        std::os::unix::fs::symlink("real", root.join("alias")).unwrap();

        let recorded = manifest_of(&root).unwrap();
        // Following it would record `alias/file` too, and a cycle would hang.
        assert!(matches!(
            recorded.paths.get("alias"),
            Some(Record::Symlink { .. })
        ));
        assert!(!recorded.paths.contains_key("alias/file"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_cycle_does_not_hang_the_walk() {
        let (_guard, root) = tree();
        std::os::unix::fs::symlink("..", root.join("bin/up")).unwrap();
        // Recording links rather than following them makes this terminate.
        assert!(manifest_of(&root).is_ok());
    }

    #[test]
    fn a_file_replaced_by_a_directory_is_a_type_change() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();

        std::fs::remove_file(root.join("README.md")).unwrap();
        std::fs::create_dir(root.join("README.md")).unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        assert_eq!(differences[0].kind, DifferenceKind::TypeChanged);
        assert!(differences[0].detail.contains("was a file"));
    }

    #[test]
    fn differences_are_reported_in_path_order() {
        let (_guard, root) = tree();
        let recorded = manifest_of(&root).unwrap();
        std::fs::write(root.join("README.md"), b"x").unwrap();
        std::fs::write(root.join("bin/node"), b"y").unwrap();
        std::fs::write(root.join("zzz"), b"z").unwrap();

        let differences = recorded.compare(&manifest_of(&root).unwrap());
        let paths: Vec<&str> = differences.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "bin/node", "zzz"]);
    }
}
