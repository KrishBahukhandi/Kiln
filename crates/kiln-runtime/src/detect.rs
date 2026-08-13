//! Reading project files safely.
//!
//! Detection runs against a repository that was just cloned, so every file it
//! touches is untrusted input. These helpers cap what they will read and never
//! propagate an error: a project whose `package.json` is a 4 GB log file should
//! make `kiln init` propose nothing, not make it fail or hang.

use std::io::Read;
use std::path::Path;

use kiln_core::VersionReq;

/// The most Kiln will read from a project file while sniffing it.
///
/// A real `package.json` or `pyproject.toml` is orders of magnitude below this.
const MAX_DETECTION_BYTES: u64 = 1024 * 1024;

/// Read a project file, giving up on anything oversized or unreadable.
pub fn read_capped(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_DETECTION_BYTES {
        return None;
    }

    let mut text = String::new();
    file.take(MAX_DETECTION_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// Read a single-value version file such as `.nvmrc` or `.python-version`.
///
/// Returns the first line that carries content, ignoring blank lines and `#`
/// comments, both of which appear in the wild.
pub fn read_version_file(path: &Path) -> Option<String> {
    let text = read_capped(path)?;
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
}

/// Translate a version requirement written for another tool.
///
/// Tries Kiln's grammar first, since it already covers the common spellings
/// (`22`, `^22`, `>=22, <23`). Failing that, falls back to the first
/// `major[.minor]` in the string and treats it as a pin — which is how a human
/// would read `~=3.11` or `>=3.11,<3.14` when asked "so, which Python?".
///
/// Returns `None` when there is no number to find at all, so the caller can fall
/// back to "uses this runtime, version unknown".
pub fn translate_requirement(raw: &str) -> Option<VersionReq> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(requirement) = VersionReq::parse(text) {
        return Some(requirement);
    }
    first_version_pin(text)
}

/// Extract the first `major[.minor]` sequence and turn it into a pin.
fn first_version_pin(text: &str) -> Option<VersionReq> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        // Take a minor component if one follows immediately.
        if index + 1 < bytes.len() && bytes[index] == b'.' && bytes[index + 1].is_ascii_digit() {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
        return VersionReq::parse(&text[start..index]).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("kiln-detect-{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Scratch(path)
        }

        fn write(&self, name: &str, contents: &str) -> std::path::PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn version_files_ignore_blank_lines_and_comments() {
        let scratch = Scratch::new("version-file");
        let path = scratch.write(".nvmrc", "\n# managed by nvm\n\n  22.14.0  \n20\n");
        assert_eq!(read_version_file(&path).as_deref(), Some("22.14.0"));
    }

    #[test]
    fn missing_files_detect_as_nothing() {
        assert!(read_capped(Path::new("/nonexistent/package.json")).is_none());
        assert!(read_version_file(Path::new("/nonexistent/.nvmrc")).is_none());
    }

    #[test]
    fn directories_are_not_readable_as_files() {
        let scratch = Scratch::new("dir");
        std::fs::create_dir_all(scratch.0.join("package.json")).unwrap();
        assert!(read_capped(&scratch.0.join("package.json")).is_none());
    }

    #[test]
    fn oversized_files_are_refused_rather_than_loaded() {
        let scratch = Scratch::new("huge");
        let path = scratch.write("package.json", &"x".repeat(2 * 1024 * 1024));
        assert!(read_capped(&path).is_none());
    }

    #[test]
    fn kiln_requirements_translate_unchanged() {
        for text in ["22", "22.14.0", "^22", ">=22, <23", "lts"] {
            let translated = translate_requirement(text).expect(text);
            assert_eq!(translated.to_string(), text);
        }
    }

    #[test]
    fn comma_ranges_are_already_kiln_syntax() {
        // PEP 440 and Kiln agree on this spelling, so it survives intact rather
        // than being collapsed to a pin.
        assert_eq!(
            translate_requirement(">=3.11,<3.14").unwrap().to_string(),
            ">=3.11, <3.14"
        );
    }

    #[test]
    fn foreign_requirements_fall_back_to_the_first_version_they_mention() {
        // PEP 440 and npm spellings Kiln does not implement.
        assert_eq!(translate_requirement("~=3.11").unwrap().to_string(), "3.11");
        assert_eq!(
            translate_requirement("^20 || ^22").unwrap().to_string(),
            "20"
        );
        assert_eq!(translate_requirement(">=18 <21").unwrap().to_string(), "18");
        assert_eq!(
            translate_requirement("v22.14.0").unwrap().to_string(),
            "22.14.0"
        );
    }

    #[test]
    fn requirements_with_no_version_translate_to_nothing() {
        for text in ["", "   ", "lts/iron", "system", "*"] {
            assert!(
                translate_requirement(text).is_none(),
                "`{text}` should not translate"
            );
        }
    }
}
