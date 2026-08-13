//! Content addresses.
//!
//! Every artifact Kiln stores is named by the cryptographic hash of its bytes.
//! Two projects that need byte-identical Node.js tarballs converge on the same
//! store entry without either of them knowing about the other, and a corrupted
//! entry is detectable without consulting anything external.
//!
//! The algorithm is carried in the value rather than assumed, so a future
//! migration does not have to reinterpret existing addresses.

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};

use crate::error::{Error, IoResultExt, Result};

/// Hash algorithms Kiln can produce and verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum HashAlgorithm {
    /// SHA-256. What upstream runtime vendors publish, and Kiln's default.
    Sha256,
}

impl HashAlgorithm {
    /// Stable lowercase identifier used in the textual form.
    pub const fn as_str(self) -> &'static str {
        match self {
            HashAlgorithm::Sha256 => "sha256",
        }
    }

    /// Length of a digest of this algorithm, in bytes.
    pub const fn output_len(self) -> usize {
        match self {
            HashAlgorithm::Sha256 => 32,
        }
    }
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HashAlgorithm {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "sha256" => Ok(HashAlgorithm::Sha256),
            other => {
                Err(Error::config(format!("unknown hash algorithm `{other}`")).expected("sha256"))
            }
        }
    }
}

/// The content address of an artifact, written `sha256:<hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest {
    algorithm: HashAlgorithm,
    bytes: Vec<u8>,
}

impl Digest {
    /// Wrap raw hash bytes, checking they have the right length for the algorithm.
    pub fn from_bytes(algorithm: HashAlgorithm, bytes: Vec<u8>) -> Result<Self> {
        if bytes.len() != algorithm.output_len() {
            return Err(Error::new(
                crate::ErrorKind::Verification,
                format!("malformed {algorithm} digest"),
            )
            .because(format!(
                "expected {} bytes, got {}",
                algorithm.output_len(),
                bytes.len()
            )));
        }
        Ok(Digest { algorithm, bytes })
    }

    /// Hash a byte slice.
    pub fn of_bytes(algorithm: HashAlgorithm, data: &[u8]) -> Self {
        match algorithm {
            HashAlgorithm::Sha256 => Digest {
                algorithm,
                bytes: Sha256::digest(data).to_vec(),
            },
        }
    }

    /// Start an incremental hash.
    ///
    /// Used while a download is in flight, so the bytes are hashed exactly once,
    /// on their way to disk, rather than written and then read back.
    pub fn hasher(algorithm: HashAlgorithm) -> Hasher {
        Hasher::new(algorithm)
    }

    /// Hash a stream without holding it in memory.
    pub fn of_reader(algorithm: HashAlgorithm, reader: &mut impl Read) -> std::io::Result<Self> {
        match algorithm {
            HashAlgorithm::Sha256 => {
                let mut hasher = Sha256::new();
                let mut buffer = vec![0u8; 64 * 1024];
                loop {
                    let read = reader.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                Ok(Digest {
                    algorithm,
                    bytes: hasher.finalize().to_vec(),
                })
            }
        }
    }

    /// Hash the contents of a file.
    pub fn of_file(algorithm: HashAlgorithm, path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut file =
            std::fs::File::open(path).io_context("Could not open file for hashing", path)?;
        Digest::of_reader(algorithm, &mut file).io_context("Could not read file for hashing", path)
    }

    /// Parse the textual form `algorithm:hex`.
    pub fn parse(text: &str) -> Result<Self> {
        let (algorithm, hex) = text.split_once(':').ok_or_else(|| {
            Error::config(format!("malformed digest `{text}`"))
                .because("a digest is written as `algorithm:hex`")
                .expected("sha256:9f86d081884c7d65…")
        })?;
        let algorithm: HashAlgorithm = algorithm.parse()?;
        let bytes = decode_hex(hex).ok_or_else(|| {
            Error::config(format!("malformed digest `{text}`"))
                .because("the digest body is not lowercase hexadecimal")
        })?;
        Digest::from_bytes(algorithm, bytes)
    }

    /// The algorithm this digest was produced with.
    pub fn algorithm(&self) -> HashAlgorithm {
        self.algorithm
    }

    /// The raw hash bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The hash body as lowercase hexadecimal, without the algorithm prefix.
    pub fn hex(&self) -> String {
        encode_hex(&self.bytes)
    }

    /// A short prefix for display. Never use this for identity.
    pub fn short(&self) -> String {
        self.hex().chars().take(12).collect()
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex())
    }
}

impl FromStr for Digest {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        Digest::parse(s)
    }
}

impl Serialize for Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Digest::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// An incremental hash over bytes that arrive a chunk at a time.
#[derive(Debug, Clone)]
pub struct Hasher {
    algorithm: HashAlgorithm,
    state: Sha256,
    bytes_seen: u64,
}

impl Hasher {
    /// Begin hashing.
    pub fn new(algorithm: HashAlgorithm) -> Self {
        Hasher {
            algorithm,
            state: match algorithm {
                HashAlgorithm::Sha256 => Sha256::new(),
            },
            bytes_seen: 0,
        }
    }

    /// Absorb a chunk.
    pub fn update(&mut self, chunk: &[u8]) {
        self.state.update(chunk);
        self.bytes_seen += chunk.len() as u64;
    }

    /// How many bytes have been absorbed so far.
    pub fn bytes_seen(&self) -> u64 {
        self.bytes_seen
    }

    /// Finish, producing the content address.
    pub fn finish(self) -> Digest {
        Digest {
            algorithm: self.algorithm,
            bytes: self.state.finalize().to_vec(),
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Decode lowercase hex. Uppercase is rejected so that a digest has exactly one
/// textual form and therefore exactly one cache path.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(text.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = hex_value(pair[0])?;
        let lo = hex_value(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SHA-256 of the empty input, from the FIPS 180-4 test vectors.
    const EMPTY_SHA256: &str =
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    /// The SHA-256 of `abc`.
    const ABC_SHA256: &str =
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn matches_known_vectors() {
        assert_eq!(
            Digest::of_bytes(HashAlgorithm::Sha256, b"").to_string(),
            EMPTY_SHA256
        );
        assert_eq!(
            Digest::of_bytes(HashAlgorithm::Sha256, b"abc").to_string(),
            ABC_SHA256
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data = vec![7u8; 300_000];
        let streamed =
            Digest::of_reader(HashAlgorithm::Sha256, &mut data.as_slice()).expect("hash stream");
        assert_eq!(streamed, Digest::of_bytes(HashAlgorithm::Sha256, &data));
    }

    #[test]
    fn text_form_round_trips() {
        let digest = Digest::of_bytes(HashAlgorithm::Sha256, b"kiln");
        let parsed = Digest::parse(&digest.to_string()).expect("round-trip");
        assert_eq!(parsed, digest);
        assert_eq!(parsed.algorithm(), HashAlgorithm::Sha256);
        assert_eq!(parsed.hex().len(), 64);
    }

    #[test]
    fn different_inputs_produce_different_addresses() {
        assert_ne!(
            Digest::of_bytes(HashAlgorithm::Sha256, b"node-22.14.0"),
            Digest::of_bytes(HashAlgorithm::Sha256, b"node-22.14.1")
        );
    }

    #[test]
    fn malformed_digests_are_rejected() {
        let cases = [
            "",
            "deadbeef",        // no algorithm
            "md5:abcd",        // unknown algorithm
            "sha256:",         // empty body
            "sha256:zz",       // not hex
            "sha256:abc",      // odd length
            "sha256:ABBA",     // uppercase: one address per digest
            "sha256:deadbeef", // right shape, wrong length
        ];
        for case in cases {
            assert!(Digest::parse(case).is_err(), "`{case}` should be rejected");
        }
    }

    #[test]
    fn byte_length_is_enforced() {
        assert!(Digest::from_bytes(HashAlgorithm::Sha256, vec![0u8; 32]).is_ok());
        assert!(Digest::from_bytes(HashAlgorithm::Sha256, vec![0u8; 31]).is_err());
        assert!(Digest::from_bytes(HashAlgorithm::Sha256, Vec::new()).is_err());
    }

    #[test]
    fn short_form_is_a_prefix_of_the_hex() {
        let digest = Digest::of_bytes(HashAlgorithm::Sha256, b"kiln");
        assert_eq!(digest.short().len(), 12);
        assert!(digest.hex().starts_with(&digest.short()));
    }

    #[test]
    fn incremental_hashing_matches_one_shot() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();

        let mut hasher = Digest::hasher(HashAlgorithm::Sha256);
        for chunk in data.chunks(97) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.bytes_seen(), data.len() as u64);
        assert_eq!(
            hasher.finish(),
            Digest::of_bytes(HashAlgorithm::Sha256, &data)
        );
    }

    #[test]
    fn an_empty_incremental_hash_matches_the_empty_digest() {
        let hasher = Digest::hasher(HashAlgorithm::Sha256);
        assert_eq!(hasher.bytes_seen(), 0);
        assert_eq!(hasher.finish().to_string(), EMPTY_SHA256);
    }

    #[test]
    fn hashing_a_file_matches_hashing_its_bytes() {
        let path = std::env::temp_dir().join(format!("kiln-digest-{}.bin", std::process::id()));
        let data = b"reproducible builds";
        std::fs::write(&path, data).unwrap();

        let from_file = Digest::of_file(HashAlgorithm::Sha256, &path).expect("hash file");
        assert_eq!(from_file, Digest::of_bytes(HashAlgorithm::Sha256, data));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn hashing_a_missing_file_names_the_path() {
        let path = std::env::temp_dir().join("kiln-digest-absent-42.bin");
        let err = Digest::of_file(HashAlgorithm::Sha256, &path).unwrap_err();
        assert_eq!(err.kind(), crate::ErrorKind::Io);
        assert!(err.reason().unwrap().contains("kiln-digest-absent-42.bin"));
    }
}
