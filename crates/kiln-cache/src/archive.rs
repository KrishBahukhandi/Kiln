//! Unpacking a verified artifact.
//!
//! Extraction runs *after* the artifact's digest has been checked against what
//! the publisher declared, so the bytes are the ones upstream shipped. What
//! extraction still has to get right is the filesystem: a tar entry can name
//! `../../etc/passwd`, or plant a symlink that a later entry writes through.
//!
//! The dangerous part is delegated: `tar::Entry::unpack_in` canonicalises every
//! destination and refuses anything landing outside the target directory, which
//! covers both attacks and is far better tested than a hand-rolled equivalent.
//!
//! What Kiln adds is a stricter *policy* on top. `tar` skips an unsafe entry and
//! carries on; Kiln stops. A digest-verified artifact from nodejs.org does not
//! contain traversal entries, so finding one means something is wrong that
//! installing the remaining nine thousand files will not fix.
//!
//! Permissions are taken from the archive with `preserve_permissions` left off,
//! which keeps the executable bit — a runtime is useless without it — while
//! dropping setuid, setgid and sticky bits. Nothing Kiln downloads has any
//! business being setuid.

use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use kiln_core::ArtifactFormat;
use kiln_core::error::{Error, IoResultExt, Result};

/// Unpack `archive` inside `workspace` and return the runtime's root directory.
///
/// `strip_components` wrapper directories are removed from the top of the tree,
/// so `node-v22.14.0-darwin-arm64/bin/node` becomes `bin/node` and no version
/// number leaks into a stored path.
pub fn extract(
    archive: &Path,
    format: ArtifactFormat,
    workspace: &Path,
    strip_components: usize,
) -> Result<PathBuf> {
    if !format.is_supported() {
        return Err(
            Error::unsupported(format!("Kiln cannot unpack {format} archives"))
                .because(format!(
                    "This build reads tar.gz and zip; the artifact is packed as {format}."
                ))
                .hint("choose a version published as a gzip tarball or a zip, or open an issue"),
        );
    }

    let raw = workspace.join("unpacked");
    if raw.exists() {
        std::fs::remove_dir_all(&raw).io_context("Could not clear the staging directory", &raw)?;
    }
    std::fs::create_dir_all(&raw).io_context("Could not create the staging directory", &raw)?;

    match format {
        ArtifactFormat::Zip => crate::zip::unpack(archive, &raw)?,
        // `is_supported` has already refused everything else.
        _ => unpack_tar_gz(archive, &raw)?,
    }
    descend(&raw, strip_components, archive)
}

fn unpack_tar_gz(archive: &Path, into: &Path) -> Result<()> {
    let file = std::fs::File::open(archive).io_context("Could not open the artifact", archive)?;
    let reader = std::io::BufReader::with_capacity(256 * 1024, file);
    let mut tar = tar::Archive::new(GzDecoder::new(reader));

    // Leaving `preserve_permissions` off masks away setuid while keeping the
    // executable bit, which is exactly the policy Kiln wants.
    tar.set_overwrite(true);

    // Entries are walked individually rather than with `Archive::unpack`,
    // because `unpack` *skips* an unsafe entry and carries on. Skipping is safe
    // but silent, and a digest-verified publisher artifact containing a
    // traversal entry is not a thing to shrug at — it means either the archive
    // or Kiln's understanding of it is wrong, and installing the rest of it
    // anyway would be the wrong instinct.
    let entries = tar
        .entries()
        .map_err(|e| unreadable(archive, &e).with_source(e))?;

    for entry in entries {
        let mut entry = entry.map_err(|e| unreadable(archive, &e).with_source(e))?;

        let path = entry
            .path()
            .map_err(|e| refused(archive, &e.to_string(), "its name is not a usable path"))?
            .into_owned();

        if let Some(problem) = unsafe_path(&path) {
            return Err(refused(archive, &path.display().to_string(), problem));
        }

        // Symlinks are legitimate and common — `bin/python3 -> python3.13` —
        // but only when they stay inside the tree.
        if let Ok(Some(link)) = entry.link_name()
            && let Some(problem) = unsafe_link(&path, &link)
        {
            return Err(refused(archive, &path.display().to_string(), problem));
        }

        let unpacked = entry
            .unpack_in(into)
            .map_err(|e| unreadable(archive, &e).with_source(e))?;
        if !unpacked {
            return Err(refused(
                archive,
                &path.display().to_string(),
                "the tar reader rejected it as unsafe",
            ));
        }
    }
    Ok(())
}

/// Why a path is not acceptable inside an archive, if it is not.
pub(crate) fn unsafe_path(path: &Path) -> Option<&'static str> {
    use std::path::Component;

    if path.is_absolute() {
        return Some("it is an absolute path");
    }
    for component in path.components() {
        match component {
            Component::ParentDir => return Some("it contains `..`"),
            Component::RootDir | Component::Prefix(_) => {
                return Some("it is anchored outside the archive");
            }
            Component::Normal(_) | Component::CurDir => {}
        }
    }
    None
}

/// Why a link target is not acceptable, if it is not.
///
/// Resolved lexically against the link's own directory: a link may point
/// anywhere within the extracted tree, and nowhere outside it.
pub(crate) fn unsafe_link(link_path: &Path, target: &Path) -> Option<&'static str> {
    use std::path::Component;

    if target.is_absolute() {
        return Some("it is a symlink to an absolute path");
    }

    let mut depth: i64 = link_path.components().count() as i64 - 1;
    for component in target.components() {
        match component {
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Some("it is a symlink pointing outside the archive");
                }
            }
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => {
                return Some("it is a symlink anchored outside the archive");
            }
        }
    }
    None
}

pub(crate) fn refused(archive: &Path, entry: &str, problem: &str) -> Error {
    Error::new(
        kiln_core::ErrorKind::Verification,
        "The artifact contains an entry Kiln will not extract",
    )
    .because(format!(
        "In {}, the entry `{entry}` was refused because {problem}.",
        archive.display()
    ))
    .hint("nothing was installed")
    .hint("this should not happen with an official artifact; please report it")
}

fn unreadable(archive: &Path, cause: &std::io::Error) -> Error {
    Error::new(
        kiln_core::ErrorKind::Verification,
        "Could not unpack the artifact",
    )
    .because(format!("{}: {cause}", archive.display()))
    .hint("the download may be truncated; run the command again")
}

/// Walk down `levels` wrapper directories.
///
/// Each level must contain exactly one directory and nothing else. If an archive
/// is shaped differently, Kiln stops rather than guessing: picking "the first
/// directory" out of an unexpected layout is how a tool silently installs the
/// wrong thing.
fn descend(root: &Path, levels: usize, archive: &Path) -> Result<PathBuf> {
    let mut current = root.to_path_buf();

    for level in 0..levels {
        let mut children: Vec<PathBuf> = std::fs::read_dir(&current)
            .io_context("Could not read the unpacked artifact", &current)?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                // macOS tarballs sometimes carry AppleDouble sidecars.
                !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("._") || n == ".DS_Store")
            })
            .collect();
        children.sort();

        let only_directory = match children.as_slice() {
            [single] if single.is_dir() => single.clone(),
            _ => {
                let names: Vec<String> = children
                    .iter()
                    .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .take(6)
                    .collect();
                return Err(Error::new(
                    kiln_core::ErrorKind::Verification,
                    "The artifact is not laid out the way Kiln expected",
                )
                .because(format!(
                    "At level {} of {}, Kiln expected exactly one directory but found {}.",
                    level + 1,
                    archive.display(),
                    if names.is_empty() {
                        "nothing".to_string()
                    } else {
                        names.join(", ")
                    }
                ))
                .hint("the upstream archive layout may have changed; please report this"));
            }
        };
        current = only_directory;
    }

    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a tar.gz in memory from `(path, contents, mode)` triples.
    ///
    /// Paths are written straight into the header's name field rather than
    /// through `append_data`, because `append_data` validates them — and the
    /// hostile archives these tests need are exactly the ones it refuses to
    /// produce. A real attacker has no such scruples, so neither does the
    /// fixture builder.
    fn make_archive(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, contents, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(*mode);
            header.set_entry_type(tar::EntryType::Regular);

            let raw = path.as_bytes();
            assert!(raw.len() < 100, "test paths stay in the short name field");
            let gnu = header.as_gnu_mut().expect("GNU header");
            gnu.name[..raw.len()].copy_from_slice(raw);

            header.set_cksum();
            builder
                .append(&header, std::io::Cursor::new(contents))
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn write_archive(directory: &Path, entries: &[(&str, &[u8], u32)]) -> PathBuf {
        let path = directory.join("artifact.tar.gz");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&make_archive(entries)).unwrap();
        path
    }

    /// A tarball shaped like Node's: one wrapper directory, a binary in `bin`.
    const NODE_SHAPED: &[(&str, &[u8], u32)] = &[
        ("node-v22.14.0-darwin-arm64/bin/node", b"#!/bin/sh\n", 0o755),
        ("node-v22.14.0-darwin-arm64/README.md", b"readme\n", 0o644),
        ("node-v22.14.0-darwin-arm64/lib/thing.js", b"x\n", 0o644),
    ];

    #[test]
    fn strips_the_wrapper_directory() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(workspace.path(), NODE_SHAPED);

        let root = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap();

        // The version number does not survive into the stored tree.
        assert!(root.join("bin/node").is_file());
        assert!(root.join("README.md").is_file());
        assert!(!root.join("node-v22.14.0-darwin-arm64").exists());
    }

    #[test]
    fn stripping_nothing_keeps_the_wrapper() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(workspace.path(), NODE_SHAPED);

        let root = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 0).unwrap();
        assert!(root.join("node-v22.14.0-darwin-arm64/bin/node").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn the_executable_bit_survives() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(workspace.path(), NODE_SHAPED);
        let root = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap();

        // A runtime whose interpreter is not executable is not a runtime.
        let mode = std::fs::metadata(root.join("bin/node"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "mode was {mode:o}");

        let plain = std::fs::metadata(root.join("README.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(plain & 0o111, 0, "data files should not become executable");
    }

    #[cfg(unix)]
    #[test]
    fn setuid_bits_are_dropped() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(
            workspace.path(),
            &[("prefix/bin/rooted", b"#!/bin/sh\n", 0o4755)],
        );
        let root = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap();

        let mode = std::fs::metadata(root.join("bin/rooted"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o4000, 0, "setuid must not survive extraction");
        assert_eq!(mode & 0o111, 0o111, "but it should still be executable");
    }

    #[test]
    fn path_traversal_entries_are_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let escape = workspace.path().join("escaped.txt");

        let archive = write_archive(
            workspace.path(),
            &[
                ("prefix/ok.txt", b"fine\n", 0o644),
                ("prefix/../../escaped.txt", b"owned\n", 0o644),
            ],
        );

        let error = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap_err();
        assert!(
            !escape.exists(),
            "nothing may be written outside the workspace"
        );

        // Loudly, not silently: a verified publisher artifact should never
        // contain one of these, so carrying on with the rest would be wrong.
        assert_eq!(error.kind(), kiln_core::ErrorKind::Verification);
        assert!(
            error.reason().unwrap().contains("`..`"),
            "{}",
            error.reason().unwrap()
        );
        assert!(
            error
                .hints()
                .iter()
                .any(|h| h.text() == "nothing was installed")
        );
    }

    #[test]
    fn absolute_paths_do_not_escape() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(
            workspace.path(),
            &[
                ("prefix/ok.txt", b"fine\n", 0o644),
                ("/tmp/kiln-absolute-escape", b"x\n", 0o644),
            ],
        );

        let error = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap_err();
        assert!(
            !Path::new("/tmp/kiln-absolute-escape").exists(),
            "an absolute entry must not be written to its literal path"
        );
        assert!(error.reason().unwrap().contains("absolute"));
    }

    #[test]
    fn an_unexpected_layout_is_refused_rather_than_guessed() {
        let workspace = tempfile::tempdir().unwrap();
        // Two top-level directories: which one is the runtime?
        let archive = write_archive(
            workspace.path(),
            &[("one/bin/node", b"x", 0o755), ("two/bin/node", b"x", 0o755)],
        );

        let error = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap_err();
        assert!(error.summary().contains("not laid out"));
        assert!(error.reason().unwrap().contains("one"));
        assert!(error.reason().unwrap().contains("two"));
    }

    #[test]
    fn an_empty_archive_is_refused() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(workspace.path(), &[]);

        let error = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap_err();
        assert!(error.reason().unwrap().contains("nothing"));
    }

    #[test]
    fn apple_double_sidecars_do_not_confuse_the_layout() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(
            workspace.path(),
            &[
                ("prefix/bin/node", b"x", 0o755),
                ("._prefix", b"junk", 0o644),
                (".DS_Store", b"junk", 0o644),
            ],
        );

        let root = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap();
        assert!(root.join("bin/node").is_file());
    }

    #[test]
    fn a_truncated_archive_is_reported_as_a_bad_download() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("artifact.tar.gz");
        let full = make_archive(NODE_SHAPED);
        std::fs::write(&path, &full[..full.len() / 2]).unwrap();

        let error = extract(&path, ArtifactFormat::TarGz, workspace.path(), 1).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::Verification);
        assert!(error.hints().iter().any(|h| h.text().contains("truncated")));
    }

    #[test]
    fn unsupported_formats_say_which_one_they_are() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(workspace.path(), NODE_SHAPED);

        // xz is the one left. It stays recognisable in a lockfile without being
        // unpackable, so a lockfile written by a future Kiln still parses here.
        let error = extract(&archive, ArtifactFormat::TarXz, workspace.path(), 1).unwrap_err();
        assert_eq!(error.kind(), kiln_core::ErrorKind::Unsupported);
        assert!(error.summary().contains("tar.xz"));
    }

    #[test]
    fn extraction_is_repeatable_into_the_same_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = write_archive(workspace.path(), NODE_SHAPED);

        // A retried install must not trip over the previous attempt's leftovers.
        for _ in 0..3 {
            let root = extract(&archive, ArtifactFormat::TarGz, workspace.path(), 1).unwrap();
            assert!(root.join("bin/node").is_file());
        }
    }
}
