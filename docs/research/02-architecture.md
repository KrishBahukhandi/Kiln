# System Architecture

Kiln 0.1.0. Rust 2024 edition, minimum supported compiler 1.88 (for let-chains).
Approximately 19,550 lines of Rust across eight crates, 37 direct dependencies
resolving to 145 total. No `unsafe` code: every crate carries
`#![forbid(unsafe_code)]`.

## Crate structure

The workspace is split so that each crate has one reason to change, and so that
the dependency arrows describe the intended layering.

| Crate | Lines | Responsibility |
| --- | --- | --- |
| `kiln-core` | 3,334 | Error type, version grammar, platform identity, digests, paths, terminal output primitives |
| `kiln-config` | 1,852 | `kiln.toml` parsing, validation, project discovery |
| `kiln-cache` | 3,223 | Content-addressed store, archive extraction (tar.gz and zip), tree manifests |
| `kiln-net` | 1,324 | HTTP client, artifact download, digest verification in flight, metadata caching |
| `kiln-runtime` | 3,177 | The `RuntimeProvider` trait and the four implementations |
| `kiln-resolver` | 2,293 | Requirement → version resolution, lockfiles, drift detection, installation, activation |
| `kiln-exec` | 902 | Process execution, `PATH` composition, signal handling |
| `kiln-cli` | 3,445 | Command surface, argument parsing, output rendering |

The important structural rule is that **nothing runtime-specific exists outside
`kiln-runtime`**. There is no `if runtime == "node"` anywhere else in the tree.
A conditional of that shape appearing in the resolver or the CLI is treated as
evidence that the `RuntimeProvider` abstraction has a hole.

## The provider abstraction

A runtime provider answers questions; it never performs I/O on the filesystem
beyond reading a project's own version files.

```
trait RuntimeProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn default_requirement(&self) -> VersionReq;
    fn supports(&self, platform: &Platform) -> bool;
    fn layout(&self) -> RuntimeLayout;
    fn releases(&self, ctx: &ProviderContext) -> Result<Vec<Release>>;
    fn artifact(&self, version: &Version, ctx: &ProviderContext) -> Result<ArtifactSpec>;
    fn detect(&self, project_root: &Path) -> Option<Evidence>;
    fn homepage(&self) -> &'static str;
}
```

**The provider declares; the installer acts.** Downloading, digest checking,
extraction, and atomic insertion into the store are shared code, written once.
A provider contributes a `RuntimeLayout` — how many wrapper directories to strip
and where the executables sit — and the shared installer does the rest. This is
what keeps the count of archive-handling code paths at one per format rather
than one per runtime.

Four providers exist: Node.js (nodejs.org), Python (python-build-standalone via
GitHub releases), Go (go.dev), and Deno (dl.deno.land).

### Provider-visible differences that shaped the abstraction

The four upstreams differ in ways that are instructive, because each difference
forced something into the shared layer:

- **Where the digest comes from.** Go's release index carries `sha256` inline,
  so one request suffices. Node.js and Python publish a separate `SHASUMS` file
  per release. Deno publishes a `.sha256sum` sidecar beside each artifact. The
  provider returns an `ArtifactSpec` containing a digest; how it obtained one is
  its own business.
- **Archive format.** Node, Python and Go ship gzip tarballs. Deno ships zip
  only, which forced a zip reader into `kiln-cache`.
- **Internal layout.** Node, Python and Go unpack to a single wrapper directory
  containing `bin/`. Deno's archive *is* the executable, with no wrapper and no
  `bin/`, which forced a second `RuntimeLayout` shape and a shared
  `bin_paths()` helper.
- **Platform coverage.** Python offers glibc and musl builds for both
  architectures (six platforms). Node and Deno offer glibc only (four). Go
  publishes one statically linked build per architecture that serves both libcs.

## Data flow

```
kiln.toml                    declared requirements
    │
    ├─ discovery ────────────► project root (walk up to filesystem root)
    │
    ▼
resolution                   requirement + platform → exact version
    │                        (from kiln.lock if present, else from the
    │                         provider's release index over HTTPS)
    ▼
kiln.lock                    exact version + URL + digest + format,
    │                        per platform
    ▼
store lookup                 is this digest already unpacked?
    │
    ├── yes ─────────────────► reuse, nothing downloaded
    │
    └── no
        ▼
    download                 to staging, hashed in flight
        ▼
    verify                   digest vs. what the publisher declared;
        │                    a mismatch destroys the bytes
        ▼
    extract                  into staging, path-safety enforced
        ▼
    manifest                 record every path, size, digest, exec bit
        ▼
    rename(2)                staging → store, atomic
        ▼
    PATH composition         store paths prepended, project environment applied
```

Only the `rename(2)` makes anything visible as installed. An installation
interrupted at any earlier point leaves the store exactly as it was and at worst
leaves a directory in `staging/` that a later run removes.

## On-disk layout

```
~/.kiln/
  store/
    sha256/
      9f/
        9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08/
          meta.toml         provider, version, source URL, digest, install time
          tree.manifest     every path in content/, as unpacked
          last-used         Unix timestamp, for garbage collection
          content/          the unpacked runtime
  staging/                  in-progress work; same filesystem as store/
  state/http/               cached release indexes, TTL-bounded
```

Two characters of the digest form a fan-out directory, giving 256 buckets. This
keeps directory listings small on filesystems that degrade when a directory
grows very wide, without burying entries so deep that the store is tedious to
inspect by hand.

Everything except `content/` sits *beside* the runtime rather than inside it, so
Kiln can record what it needs without adding files to a runtime that did not
ship them.

`staging/` is a sibling of `store/` rather than living in `/tmp` because
promotion must be a `rename(2)` within one filesystem. A cross-device copy would
open a window in which a partially written artifact is visible under a name
claiming it is complete.

## Path safety, because the store is not a sandbox

Extraction runs *after* the digest check, so the bytes are the ones the
publisher shipped. What extraction must still get right is the filesystem: an
archive entry can name `../../etc/passwd`, or plant a symlink that a later entry
writes through.

Kiln refuses, rather than skips, three classes of entry: absolute paths, paths
containing `..`, and symlinks whose target resolves outside the extraction root.
The distinction matters. The `tar` crate's bulk `unpack` *skips* an unsafe entry
and continues, which is safe but silent — and a digest-verified artifact from
an official host containing a traversal entry means something is wrong that
installing the remaining nine thousand files will not fix.

Permissions are taken from the archive with `preserve_permissions` off, which
keeps the executable bit (a runtime is useless without it) while masking away
setuid, setgid and sticky bits. Nothing Kiln downloads has any business being
setuid.

The zip reader applies the identical policy through the same three functions,
which is the stated reason it was written rather than adopted from a library.

## Version grammar

Deliberately **not** Cargo's semantics, because the audience's expectations
differ. In Kiln, `22.14.0` means exactly 22.14.0 — not "22.14.0 or any
compatible later version". A developer writing a full version into a manifest
that pins a development environment means that version.

| Form | Meaning |
| --- | --- |
| `22.14.0` | exactly this version |
| `22.14` | newest patch of 22.14 |
| `22` | newest release of the 22 line |
| `^22.14.0` | caret, explicit and opt-in |
| `~22.14.0` | tilde, explicit and opt-in |
| `>=22, <23` | range |
| `lts` | alias, resolved by the provider |

`Version` implements `PartialEq` and `Hash` manually, both excluding build
metadata, because deriving `Hash` alongside a hand-written `PartialEq` that
ignored build metadata would violate the `Hash`/`Eq` contract — two values equal
to each other hashing differently.

## The lockfile

Keyed by platform, so one file serves a whole team:

```toml
version = 1
generated_by = "kiln 0.1.0"

[project]
name = "node-app"

[platform.linux-x86_64-gnu.runtime.node]
provider = "node"
requirement = "22.14.0"
version = "22.14.0"

[platform.linux-x86_64-gnu.runtime.node.artifact]
url = "https://nodejs.org/dist/v22.14.0/node-v22.14.0-linux-x64.tar.gz"
digest = "sha256:9d942932535988091034dc94cc5f42b6dc8784d6366df3a36c4c9ccb3996f0c2"
format = "tar.gz"
```

Platform keys are `{os}-{arch}[-{libc}]`. `kiln lock --all-platforms` resolves
for every platform Kiln supports, so a developer on macOS produces a file a
Linux runner installs from without resolving again — meaning CI installs what
was reviewed, not whatever was newest when CI ran.

`BTreeMap` is used throughout for anything reaching the lockfile or program
output, so resolution is a function of the manifest rather than of hash
iteration order.

Drift detection (`kiln lock --check`) is entirely offline and structural: it
compares the manifest's requirements against the lockfile's recorded
requirements, and reports one of three states per runtime — not locked,
requirement changed, or no longer required. It never resolves, so it is usable
as a fast CI gate.

## Error model

One `Error` type, boxed to a single pointer so that `Result<T>` stays cheap on
the success path. Every error carries up to four parts:

- **What happened** — the summary.
- **Why** — the reason.
- **What was expected** — for configuration errors, the accepted forms.
- **What to do** — hints, and a runnable command where one exists.

Configuration errors carry a source location and render a code frame with a
caret under the offending token. This is why validation happens inside
`Deserialize` implementations rather than in a later pass: the TOML parser
supplies byte spans at deserialisation time, and a check moved to a later pass
loses the location.

Exit codes are stable and distinct, so a script can branch on the kind of
failure:

| Code | Meaning |
| --- | --- |
| 0 | success |
| 2 | invalid configuration |
| 3 | not found (no project, unknown runtime) |
| 4 | unsupported platform |
| 5 | network failure |
| 6 | artifact failed verification |
| 7 | filesystem failure |
| 8 | conflicts with existing state |
| 9 | not implemented |
| 70 | internal error |
| 130 | interrupted |

## Concurrency model

There is no async runtime. HTTP is blocking (`ureq` over `rustls`), which keeps
`RuntimeProvider` object-safe and the dependency tree small.

Two places use threads:

- **Installation** may fetch several runtimes at once, bounded, opt-in via
  `--jobs`. Results are sorted back into manifest order before the first error
  is returned, so a failing `kiln.toml` fails identically regardless of which
  thread lost.
- **Each download** runs on a worker thread while the calling thread watches a
  byte counter, which is how the stall timeout is enforced. See
  `05-engineering-findings.md`.
