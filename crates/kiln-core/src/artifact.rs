//! How a runtime's bytes are packaged and laid out.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Archive formats runtime vendors publish.
///
/// Kiln reads `tar.gz` and `zip`. Zip is not optional — Deno and Bun publish
/// nothing else, so a tar-only Kiln could not install them at all.
///
/// `tar.xz` is deliberately left out. Those downloads are roughly half the size,
/// but decompressing xz needs either a C library or a much slower pure-Rust one,
/// and every runtime that ships xz ships a gzip tarball beside it. Paying a few
/// seconds of download to keep the build dependency-free is the right way round
/// for a tool people install from source. It stays in this enum so that a
/// lockfile mentioning it still parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ArtifactFormat {
    /// gzip-compressed tar. What most runtimes ship.
    #[serde(rename = "tar.gz")]
    TarGz,
    /// xz-compressed tar. Recognised in a lockfile, not yet unpacked.
    #[serde(rename = "tar.xz")]
    TarXz,
    /// zip archive. Some publishers ship nothing else — Deno and Bun both.
    Zip,
}

impl ArtifactFormat {
    /// The conventional file extension, without a leading dot.
    pub const fn extension(self) -> &'static str {
        match self {
            ArtifactFormat::TarGz => "tar.gz",
            ArtifactFormat::TarXz => "tar.xz",
            ArtifactFormat::Zip => "zip",
        }
    }

    /// Whether this build can unpack the format.
    pub const fn is_supported(self) -> bool {
        matches!(self, ArtifactFormat::TarGz | ArtifactFormat::Zip)
    }

    /// Guess the format from a URL or filename.
    pub fn from_filename(name: &str) -> Option<Self> {
        let name = name.rsplit('/').next().unwrap_or(name);
        if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(ArtifactFormat::TarGz)
        } else if name.ends_with(".tar.xz") {
            Some(ArtifactFormat::TarXz)
        } else if name.ends_with(".zip") {
            Some(ArtifactFormat::Zip)
        } else {
            None
        }
    }
}

impl fmt::Display for ArtifactFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

impl FromStr for ArtifactFormat {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "tar.gz" | "tgz" => Ok(ArtifactFormat::TarGz),
            "tar.xz" => Ok(ArtifactFormat::TarXz),
            "zip" => Ok(ArtifactFormat::Zip),
            other => Err(Error::config(format!("unknown artifact format `{other}`"))
                .expected("tar.gz, tar.xz or zip")),
        }
    }
}

/// Where the useful parts of a runtime sit inside its unpacked archive.
///
/// Providers declare this rather than implementing extraction themselves, so
/// there is exactly one piece of tar-handling code in Kiln to get right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeLayout {
    /// Leading path components to drop while unpacking.
    ///
    /// Almost every runtime tarball wraps everything in one directory named
    /// after the release (`node-v22.14.0-darwin-arm64/`), which would otherwise
    /// leak a version number into every stored path.
    pub strip_components: usize,
    /// Directories, relative to the unpacked root, that hold executables.
    ///
    /// Joined onto the store entry to build `PATH`.
    pub bin_dirs: &'static [&'static str],
}

impl RuntimeLayout {
    /// The common case: one wrapper directory, executables in `bin`.
    pub const UNIX_PREFIX: RuntimeLayout = RuntimeLayout {
        strip_components: 1,
        bin_dirs: &["bin"],
    };

    /// A single executable at the root of the archive, with no wrapper.
    ///
    /// Deno and Bun both ship this way: the archive *is* the binary. The empty
    /// bin directory means "the content root itself", which
    /// [`RuntimeLayout::bin_paths`] resolves without leaving a stray separator
    /// in `PATH`.
    pub const FLAT: RuntimeLayout = RuntimeLayout {
        strip_components: 0,
        bin_dirs: &[""],
    };

    /// The directories to put on `PATH` for a runtime unpacked at `content`.
    ///
    /// Lives here rather than at each call site because there are two of them —
    /// installing and activating — and a runtime whose `PATH` differs depending
    /// on which command composed it would be a genuinely miserable bug.
    pub fn bin_paths(&self, content: &Path) -> Vec<PathBuf> {
        self.bin_dirs
            .iter()
            .map(|dir| {
                if dir.is_empty() {
                    content.to_path_buf()
                } else {
                    content.join(dir)
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_round_trip_through_their_extension() {
        for format in [
            ArtifactFormat::TarGz,
            ArtifactFormat::TarXz,
            ArtifactFormat::Zip,
        ] {
            assert_eq!(
                format.extension().parse::<ArtifactFormat>().unwrap(),
                format
            );
        }
    }

    #[test]
    fn the_formats_publishers_actually_use_can_be_unpacked() {
        assert!(ArtifactFormat::TarGz.is_supported());
        assert!(ArtifactFormat::Zip.is_supported());
        // xz is still recognised in a lockfile without being unpackable, so a
        // lockfile written elsewhere stays readable here.
        assert!(!ArtifactFormat::TarXz.is_supported());
    }

    #[test]
    fn formats_are_recognised_from_real_artifact_urls() {
        let cases = [
            (
                "https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-arm64.tar.gz",
                Some(ArtifactFormat::TarGz),
            ),
            (
                "https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-arm64.tar.xz",
                Some(ArtifactFormat::TarXz),
            ),
            (
                "cpython-3.13.5+20250612-aarch64-apple-darwin-install_only.tar.gz",
                Some(ArtifactFormat::TarGz),
            ),
            ("node-v22.14.0-win-x64.zip", Some(ArtifactFormat::Zip)),
            ("something.tgz", Some(ArtifactFormat::TarGz)),
            ("node-v22.14.0.pkg", None),
            ("", None),
        ];
        for (name, expected) in cases {
            assert_eq!(ArtifactFormat::from_filename(name), expected, "for {name}");
        }
    }

    #[test]
    fn unknown_formats_list_the_known_ones() {
        let error = "rar".parse::<ArtifactFormat>().unwrap_err();
        assert!(error.expectation().unwrap().contains("tar.gz"));
    }

    #[test]
    fn the_unix_prefix_layout_strips_the_version_directory() {
        assert_eq!(RuntimeLayout::UNIX_PREFIX.strip_components, 1);
        assert_eq!(RuntimeLayout::UNIX_PREFIX.bin_dirs, ["bin"]);
    }

    #[test]
    fn a_flat_layout_puts_the_content_root_itself_on_the_path() {
        let content = Path::new("/store/sha256/ab/abcd/content");
        // Not `content/` with a trailing separator, and not `content/.` — both
        // work, and both look like a bug in `kiln list`.
        assert_eq!(
            RuntimeLayout::FLAT.bin_paths(content),
            [PathBuf::from("/store/sha256/ab/abcd/content")]
        );
    }

    #[test]
    fn a_prefix_layout_appends_its_bin_directory() {
        let content = Path::new("/store/x/content");
        assert_eq!(
            RuntimeLayout::UNIX_PREFIX.bin_paths(content),
            [PathBuf::from("/store/x/content/bin")]
        );
    }
}
