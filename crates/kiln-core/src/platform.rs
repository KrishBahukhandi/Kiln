//! Operating system, CPU architecture and libc detection.
//!
//! Artifacts are platform-specific, so the platform is part of a project's
//! identity: it appears in `kiln.lock`, in cache keys and in error messages.
//! Nothing in Kiln may assume x86-64 or Linux.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Operating systems Kiln knows how to name.
///
/// `Windows` is present so the type system stays honest about what a platform
/// key can contain. Runtime support for it is future work; see
/// [`Platform::is_supported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    /// macOS (Darwin).
    MacOs,
    /// Linux.
    Linux,
    /// Windows. Recognised, not yet supported.
    Windows,
}

impl Os {
    /// Stable lowercase identifier used in platform keys and lockfiles.
    pub const fn as_str(self) -> &'static str {
        match self {
            Os::MacOs => "macos",
            Os::Linux => "linux",
            Os::Windows => "windows",
        }
    }

    /// Human-facing name for terminal output.
    pub const fn display_name(self) -> &'static str {
        match self {
            Os::MacOs => "macOS",
            Os::Linux => "Linux",
            Os::Windows => "Windows",
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// CPU architectures Kiln can select artifacts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// 64-bit x86, also called amd64.
    X86_64,
    /// 64-bit ARM, also called arm64.
    Aarch64,
}

impl Arch {
    /// Stable lowercase identifier used in platform keys and lockfiles.
    pub const fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    /// Human-facing name, using the spelling vendors publish artifacts under.
    pub const fn display_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "arm64",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// C library flavour. Only meaningful on Linux, where prebuilt binaries are
/// usually linked against exactly one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Libc {
    /// GNU libc.
    Gnu,
    /// musl libc, as used by Alpine.
    Musl,
}

impl Libc {
    /// Stable lowercase identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Libc::Gnu => "gnu",
            Libc::Musl => "musl",
        }
    }
}

impl fmt::Display for Libc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The host Kiln is running on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Platform {
    /// Operating system.
    pub os: Os,
    /// CPU architecture.
    pub arch: Arch,
    /// C library flavour. `None` outside Linux, where the notion does not apply.
    pub libc: Option<Libc>,
}

impl Platform {
    /// Construct a platform descriptor directly. Useful in tests and when
    /// reading a lockfile written on another machine.
    pub const fn new(os: Os, arch: Arch, libc: Option<Libc>) -> Self {
        Platform { os, arch, libc }
    }

    /// Detect the current host.
    ///
    /// Fails on targets Kiln has no artifact naming scheme for, rather than
    /// guessing and downloading something that cannot run.
    pub fn detect() -> Result<Self> {
        let os = match std::env::consts::OS {
            "macos" => Os::MacOs,
            "linux" => Os::Linux,
            "windows" => Os::Windows,
            other => {
                return Err(Error::unsupported(format!(
                    "Kiln does not support the `{other}` operating system"
                ))
                .because("Kiln has no artifact naming scheme for this platform")
                .expected("macOS or Linux")
                .hint("track platform support in the project roadmap"));
            }
        };

        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            other => {
                return Err(Error::unsupported(format!(
                    "Kiln does not support the `{other}` CPU architecture"
                ))
                .because("runtime vendors do not publish prebuilt artifacts for it")
                .expected("x86_64 or aarch64"));
            }
        };

        let libc = match os {
            Os::Linux => Some(detect_libc(Path::new("/"))),
            _ => None,
        };

        Ok(Platform { os, arch, libc })
    }

    /// Whether this release of Kiln can install runtimes for this platform.
    ///
    /// Detection succeeds on Windows so that `kiln doctor` can explain the
    /// situation clearly; installation refuses.
    pub fn is_supported(&self) -> bool {
        matches!(self.os, Os::MacOs | Os::Linux)
    }

    /// Every platform this release can install runtimes for.
    ///
    /// The set a lockfile can cover, and therefore the set `kiln lock
    /// --all-platforms` walks. Not every runtime exists for every entry — Node
    /// publishes no musl builds — which is a fact about the runtime, not about
    /// the platform, so it belongs to the provider rather than to this list.
    pub fn all_supported() -> Vec<Platform> {
        let mut platforms = Vec::new();
        for arch in [Arch::Aarch64, Arch::X86_64] {
            platforms.push(Platform::new(Os::MacOs, arch, None));
        }
        for arch in [Arch::Aarch64, Arch::X86_64] {
            for libc in [Libc::Gnu, Libc::Musl] {
                platforms.push(Platform::new(Os::Linux, arch, Some(libc)));
            }
        }
        platforms
    }

    /// Stable key used in lockfiles and cache paths, e.g. `linux-x86_64-gnu`.
    pub fn key(&self) -> String {
        match self.libc {
            Some(libc) => format!("{}-{}-{}", self.os, self.arch, libc),
            None => format!("{}-{}", self.os, self.arch),
        }
    }

    /// Parse a key produced by [`Platform::key`].
    pub fn from_key(key: &str) -> Result<Self> {
        let mut parts = key.split('-');
        let os = match parts.next() {
            Some("macos") => Os::MacOs,
            Some("linux") => Os::Linux,
            Some("windows") => Os::Windows,
            _ => return Err(bad_key(key)),
        };
        let arch = match parts.next() {
            Some("x86_64") => Arch::X86_64,
            Some("aarch64") => Arch::Aarch64,
            _ => return Err(bad_key(key)),
        };
        let libc = match parts.next() {
            None => None,
            Some("gnu") => Some(Libc::Gnu),
            Some("musl") => Some(Libc::Musl),
            Some(_) => return Err(bad_key(key)),
        };
        if parts.next().is_some() {
            return Err(bad_key(key));
        }
        Ok(Platform { os, arch, libc })
    }
}

impl fmt::Display for Platform {
    /// Human-facing form, e.g. `macOS arm64` or `Linux x86_64 (musl)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.os.display_name(), self.arch.display_name())?;
        if let Some(libc) = self.libc {
            write!(f, " ({libc})")?;
        }
        Ok(())
    }
}

fn bad_key(key: &str) -> Error {
    Error::config(format!("unrecognised platform key `{key}`")).expected(
        "os-arch, optionally followed by a libc, e.g. `macos-aarch64` or `linux-x86_64-gnu`",
    )
}

/// Decide which libc a Linux host uses, relative to `root`.
///
/// musl systems ship their dynamic loader as `/lib/ld-musl-<arch>.so.1`; glibc
/// systems do not. Alpine's release marker is checked as a second signal. This
/// is cheap and does not shell out to `ldd`, which would mean executing a
/// program to answer a question about the filesystem.
fn detect_libc(root: &Path) -> Libc {
    let lib = root.join("lib");
    if let Ok(entries) = std::fs::read_dir(&lib) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("ld-musl-") {
                return Libc::Musl;
            }
        }
    }
    if root.join("etc/alpine-release").exists() {
        return Libc::Musl;
    }
    Libc::Gnu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_succeeds_on_the_host() {
        let platform = Platform::detect().expect("host platform should be recognised");
        assert!(platform.is_supported(), "test host must be macOS or Linux");
        if platform.os == Os::Linux {
            assert!(platform.libc.is_some(), "Linux platforms carry a libc");
        } else {
            assert!(platform.libc.is_none(), "libc is Linux-only");
        }
    }

    #[test]
    fn keys_round_trip() {
        let cases = [
            Platform::new(Os::MacOs, Arch::Aarch64, None),
            Platform::new(Os::MacOs, Arch::X86_64, None),
            Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Gnu)),
            Platform::new(Os::Linux, Arch::Aarch64, Some(Libc::Musl)),
        ];
        for platform in cases {
            let key = platform.key();
            assert_eq!(Platform::from_key(&key).unwrap(), platform, "key: {key}");
        }
    }

    #[test]
    fn known_keys_have_the_documented_spelling() {
        assert_eq!(
            Platform::new(Os::MacOs, Arch::Aarch64, None).key(),
            "macos-aarch64"
        );
        assert_eq!(
            Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Musl)).key(),
            "linux-x86_64-musl"
        );
    }

    #[test]
    fn the_supported_set_is_complete_and_distinct() {
        let all = Platform::all_supported();

        // Two macOS architectures, and four Linux ones once libc is counted.
        assert_eq!(all.len(), 6);
        assert!(all.iter().all(Platform::is_supported));

        let keys: std::collections::BTreeSet<String> = all.iter().map(Platform::key).collect();
        assert_eq!(keys.len(), all.len(), "keys must be distinct");
        for expected in [
            "macos-aarch64",
            "macos-x86_64",
            "linux-aarch64-gnu",
            "linux-aarch64-musl",
            "linux-x86_64-gnu",
            "linux-x86_64-musl",
        ] {
            assert!(keys.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn the_host_is_one_of_the_supported_platforms() {
        // `kiln lock --all-platforms` has to cover the machine running it.
        let host = Platform::detect().expect("host platform");
        assert!(Platform::all_supported().contains(&host), "host: {host}");
    }

    #[test]
    fn malformed_keys_are_rejected() {
        for key in [
            "",
            "macos",
            "plan9-x86_64",
            "linux-sparc",
            "linux-x86_64-uclibc",
            "linux-x86_64-gnu-extra",
        ] {
            assert!(
                Platform::from_key(key).is_err(),
                "`{key}` should be rejected"
            );
        }
    }

    #[test]
    fn windows_is_named_but_not_supported() {
        let windows = Platform::new(Os::Windows, Arch::X86_64, None);
        assert!(!windows.is_supported());
        assert_eq!(windows.key(), "windows-x86_64");
    }

    #[test]
    fn display_is_human_readable() {
        assert_eq!(
            Platform::new(Os::MacOs, Arch::Aarch64, None).to_string(),
            "macOS arm64"
        );
        assert_eq!(
            Platform::new(Os::Linux, Arch::X86_64, Some(Libc::Musl)).to_string(),
            "Linux x86_64 (musl)"
        );
    }

    #[test]
    fn libc_detection_reads_the_loader_name() {
        let root = tempdir();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        assert_eq!(detect_libc(&root), Libc::Gnu);

        std::fs::write(root.join("lib/ld-musl-x86_64.so.1"), b"").unwrap();
        assert_eq!(detect_libc(&root), Libc::Musl);
    }

    #[test]
    fn alpine_marker_implies_musl() {
        let root = tempdir();
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::write(root.join("etc/alpine-release"), b"3.20.0\n").unwrap();
        assert_eq!(detect_libc(&root), Libc::Musl);
    }

    /// A throwaway directory that lives for the duration of the process.
    fn tempdir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("kiln-platform-test-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
