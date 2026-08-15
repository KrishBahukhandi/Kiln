# Architecture

This document explains how Kiln is put together and, more usefully, why the
boundaries are where they are. It describes the code as it exists after Phase 2, and marks what is designed
but not yet built.

## The pipeline

```
        kiln.toml                        kiln.lock
            │                                │
   ┌────────▼────────┐                       │
   │   kiln-config   │  parse, validate      │
   └────────┬────────┘                       │
            │  Manifest                      │
   ┌────────▼────────┐                       │
   │  kiln-resolver  │  plan: names, platform│   (offline)
   └────────┬────────┘                       │
            │  PlannedRuntime                │
   ┌────────▼────────┐◄──────────────────────┘
   │  kiln-runtime   │  which version, which artifact
   └────────┬────────┘     via kiln-net, unless the lockfile answered
            │  ArtifactSpec { url, digest, format }
   ┌────────▼────────┐
   │    kiln-net     │  download, hashing in flight
   └────────┬────────┘
            │  verified archive
   ┌────────▼────────┐
   │   kiln-cache    │  extract, then rename(2) into the store
   └────────┬────────┘
            │  StoreEntry
   ┌────────▼────────┐
   │   kiln-exec     │  compose PATH and variables
   └────────┬────────┘
            │  Environment
   ┌────────▼────────┐
   │  child process  │  kiln run / kiln shell
   └─────────────────┘

   kiln-core sits underneath all of it: errors, versions, platforms,
   digests, artifact formats, paths, terminal output.
```

Each stage narrows uncertainty. The manifest may be loose (`node = "22"`); the
plan knows which provider will answer it; resolution knows the exact version; the
store knows the exact bytes. `kiln.lock` is a snapshot of the last two.

## Crate boundaries

Eight crates is more than a small tool needs, and the split is justified by
responsibility rather than by size. The test is whether a change to one forces a
change to another.

| Crate | Depends on | Owns |
| --- | --- | --- |
| `kiln-core` | — | `Error`, `Version`, `VersionReq`, `Platform`, `Digest`, `ArtifactFormat`, `KilnPaths`, `Ui` |
| `kiln-config` | core | `Manifest`, parsing, validation, discovery, generation |
| `kiln-net` | core | `Http`, `Download`, `MetadataCache` |
| `kiln-cache` | core | `ContentStore`, archive extraction |
| `kiln-runtime` | core, net | `RuntimeProvider`, `Registry`, Node and Python |
| `kiln-resolver` | core, config, net, cache, runtime | `plan`, `resolve`, `install`, `Lockfile` |
| `kiln-exec` | core, config | `Environment`, program resolution, spawning |
| `kiln-cli` | all of them | argument parsing, dispatch, presentation |

### Why `kiln-net` exists

One crate, and only one, can make an HTTP request. That turns "does this command
touch the network?" into a question answerable by reading `Cargo.toml`, and it
gives `--offline` a single place to be enforced rather than a discipline every
provider has to remember.

It is also where the blocking-versus-async decision lives. Kiln uses a blocking
client and has no async runtime: `RuntimeProvider` has to stay object-safe for
the registry and for a future plugin boundary, which async-fn-in-trait does not
allow without boxing every call, and a CLI that answers `kiln version` in two
milliseconds should not be constructing an executor to do it.

### Why `kiln-config` does not know what a runtime is

`node = "22"` and `frobnicator = "22"` are equally well-formed to the config
crate. It performs *syntactic* validation only: is this a valid name, a valid
requirement, a legal environment variable, a command that can be split into an
argv.

Deciding that only `node` names a real provider happens in `kiln-resolver`, which
depends on both `kiln-config` and `kiln-runtime`. The payoff is that adding a
runtime never touches the configuration layer, and the configuration layer can be
tested without a provider registry.

### Why `kiln-core` has no Kiln dependencies

Everything else agrees on the types in `kiln-core`. If it depended on any of
them, the dependency graph would have a cycle waiting to happen and every crate
would end up importable from every other.

## The error system

One error type, used everywhere:

```rust
pub struct Error(Box<ErrorInner>);   // pointer-sized

struct ErrorInner {
    kind: ErrorKind,                 // decides the exit code
    summary: String,                 // what happened
    reason: Option<String>,          // why
    expected: Option<String>,        // what Kiln expected
    location: Option<SourceLocation>,// where, for config errors
    hints: Vec<Hint>,                // what to do next
    source: Option<Box<dyn Error>>,  // the underlying cause, shown with -v
}
```

**Why not `anyhow` or `thiserror`.** The four-part shape above is a contract with
the presentation layer, and it is the reason Kiln's errors read the way they do.
`anyhow` erases structure into a string; `thiserror` encourages a different enum
per crate, which makes a uniform presentation impossible to enforce. Hand-writing
one type costs about two hundred lines and buys the product requirement.

**Why the payload is boxed.** A structured diagnostic is a large value — several
strings and a vector — and it is on the cold path by definition. Boxing keeps
`Result<T, Error>` cheap for the ninety-nine percent of calls that succeed.

**Formatting is separate from printing.** `kiln_core::ui::format_error` is a pure
function, so the error presentation is unit-tested rather than eyeballed.

### Carrying structure through serde

Configuration values are validated inside their `Deserialize` implementations —
`VersionReq`, `CommandSpec`, `EnvVarName` and the rest. That placement is what
makes the TOML parser hand back the byte span of the offending token, which is
what lets Kiln draw this:

```
  ┌─ kiln.toml:5:8
  │
5 │ node = "banana"
  │        ^^^^^^^^ unsupported version requirement `banana`
```

The cost is that a rich error has to travel as a flat string through
`serde::de::Error::custom`. `to_serde_message` and `decode_serde_message` in
`kiln-core` define that wire format so the structure survives the round trip.
Both directions are tested.

## Versions

Kiln owns its requirement grammar rather than reusing Cargo's, because Cargo's
default is wrong here: `22.14.0` in a Cargo manifest means "or any compatible
later release". In `kiln.toml` it must mean 22.14.0.

```rust
enum VersionReq {
    Exact(Version),          // 22.14.0
    Pinned(PartialVersion),  // 22, 22.14
    Caret(PartialVersion),   // ^22.14
    Tilde(PartialVersion),   // ~22.14
    Range(Vec<Comparator>),  // >=22, <23
    Alias(VersionAlias),     // lts, latest
}
```

Two rules exist to prevent surprise rather than to be clever:

- **Pre-releases are never selected** unless the requirement names one with the
  same `major.minor.patch`.
- **`*` is rejected.** It is not a version requirement, it is the absence of one.
  `latest` is available and gets recorded in the lockfile.

`Version` implements `PartialEq` and `Hash` by hand so that build metadata is
excluded from both. A derived `Hash` would include it and quietly break every map
keyed by a version.

## The content-addressed store

```
~/.kiln/
  store/sha256/<first two hex>/<full hex>/
    meta.toml       provider, version, source URL, artifact size
    content/        the unpacked runtime
  staging/          in-flight downloads and extraction
  state/            cached release indexes
```

**Why content addressing.** Sharing becomes automatic — two projects needing the
same bytes converge on one directory with no coordination — and corruption
becomes detectable from nothing but the bytes on disk.

**Why an entry is named by its *archive's* digest.** Hashing an unpacked
directory tree needs a canonical traversal, a decision about metadata, and a
commitment that outlives every entry ever written. The archive's digest is
already published and attested to by the vendor, and the unpacked tree is a
deterministic function of it — so Kiln verifies the thing upstream actually
signs for, and the store never had to commit to a tree-hashing scheme to exist.

**Why `staging/` is a sibling of `store/`.** Promoting a verified artifact must be
a `rename(2)` within one filesystem, which is atomic. Staging under `/tmp` would
make it a cross-device copy, opening a window where a half-written artifact is
visible under a name that says it is complete.

**Why `content/` and `meta.toml` are separate.** Provenance is recorded beside
the runtime rather than inside it, so `kiln cache list` can say where an entry
came from without contaminating the runtime with files it did not ship. The
`content/` directory is also what makes a half-written entry detectable: an entry
directory without one is a failed install, never a complete one.

**Why path traversal is impossible rather than prevented.** `path_for` is a pure
function of a `Digest`, and a `Digest` only parses from lowercase hexadecimal. No
component derived from one can contain a separator or `..`. Uppercase hex is
rejected so that each digest has exactly one path.

Implemented: layout, lookup, enumeration, size, extraction, and atomic
insertion that is idempotent and safe against a concurrent Kiln installing the
same artifact — whoever loses the race discards their copy and uses the winner's,
which is sound precisely because the store is content-addressed.

Garbage collection and verification are both implemented.

### What "this entry is intact" means

Because an entry is named by its archive's digest, the name cannot answer
whether the unpacked tree still matches — the archive is gone. So Kiln records
the answer at the one moment it is knowable: immediately after extracting an
archive whose published digest has just been checked, it walks the result and
writes `tree.manifest` beside `content/`. `kiln cache verify` walks the tree
again and compares.

The manifest records, per path: file size, the sha256 of the contents, whether
any execute bit is set, and a symlink's target unfollowed. Directories are
recorded so that losing an empty one is still noticed.

It deliberately records neither modification times nor full permission bits.
Neither survives a `cp -a`, a restored backup, or a different `tar`, and neither
affects whether a runtime works — so recording them would make verification fail
for reasons nobody could act on, which is how a check gets ignored. The
executable bit is kept because losing it is the one permission change that
actually breaks a runtime, and it is invisible to a content hash.

This detects corruption, not tampering. The manifest lives beside the tree it
describes, so whatever can rewrite one can rewrite the other. Detecting a
deliberate substitution needs an attestation from outside the store; see
`SECURITY.md`.

### Why collection goes by age and not by reachability

The question collection wants to answer is "which entries does some project
still need?". Kiln cannot answer it, and the reason is a design choice rather
than a gap: Kiln keeps no registry of projects. A lockfile on an unplugged
drive, a checkout under a colleague's home directory, or a repository nobody has
cloned yet are all invisible to it, and inventing a registry to fix that would
make a deliberately stateless tool stateful.

So Kiln records when each entry was last put to use and evicts by age, which
answers a slightly different but much more honest question: *has anything needed
this lately?*

Use is recorded by Kiln rather than read from `atime`. `relatime` is the default
on Linux and `noatime` is common, so access times can be hours stale or frozen
entirely — and collection that deleted a runtime someone uses daily would be
worse than no collection at all. The write is bounded to once per entry per
hour, and happens only in `kiln run`, `kiln shell` and `kiln install`. Read-only
commands deliberately do not count as use, or nothing would ever be collectable
on a machine where `kiln list` runs from a shell prompt.

Being wrong is cheap in exactly one direction — a needless eviction costs a
re-download, a needless retention costs disk — so the default threshold is
generous and deletion requires `--force`.

## Runtime providers

```rust
pub trait RuntimeProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn kind(&self) -> RuntimeKind;
    fn default_requirement(&self) -> VersionReq;
    fn supports(&self, platform: &Platform) -> bool;
    fn detect(&self, project_root: &Path) -> Option<Evidence>;
    fn detect_tools(&self, project_root: &Path) -> Vec<ToolEvidence>;
    fn homepage(&self) -> &'static str;
}
```

This is the only place runtime-specific knowledge is allowed to live. Adding Go
means adding one file and one line in `Registry::builtin`.

**Detection is lenient; configuration is strict.** An `engines.node` value Kiln
cannot translate makes the project "uses Node, version unknown" rather than an
error, because `kiln init` produces a proposal a human approves. The same string
inside `kiln.toml` is rejected outright.

**`kiln init` narrows compatibility statements into pins.** `engines.node: ">=22"`
says what a project *tolerates*. Copied verbatim it would resolve to a different
major release every year — the drift Kiln exists to remove — so it becomes
`node = "22"`, and the proposal says so before anything is written.

### What a provider does not do

It does not download, verify, or extract. It answers three questions —
*what versions exist*, *where is this one and what must it hash to*, and *how is
the archive laid out* — and the installer does the rest. That keeps the
security-critical code in one place instead of once per runtime, and it is why
adding Go is a file rather than a project.

`resolve` is a provided method on the trait, not something each provider writes.
It short-circuits an exact requirement without fetching the release index at all,
so `node = "22.14.0"` costs one small checksum request rather than a
quarter-megabyte of JSON — which also means a fully pinned project depends on
less upstream infrastructure staying up.

### What adding a runtime actually costs

Go was added after Node.js and Python, and the claim held: one file in
`providers/`, one line in `Registry::builtin`, and nothing else in the workspace
changed. Each provider's oddities stayed inside its own file —

| | Node.js | Python | Go |
| --- | --- | --- | --- |
| Version index | `dist/index.json` | `SHA256SUMS` per release | `dl/?mode=json` |
| Checksums | separate `SHASUMS256.txt` | same file as the index | **inline in the index** |
| Requests to resolve | 2 | 2 | 1 |
| musl | not published | separate artifact | same artifact (static) |
| Version quirk | `lts` is `false` or a string | none | `go1.20`, `go1.27rc3` |

— which is the point of the abstraction. Go's is the sharpest example: its
filenames use the upstream string verbatim, so `go1.20`'s tarball is
`go1.20.darwin-arm64.tar.gz` and not `go1.20.0...`. The provider takes filenames
from the index rather than rebuilding them from a normalised version, and that
decision is invisible to every other crate.

### `ServiceProvider`

Still unwritten, for the same reason the download methods were unwritten in Phase
0. `[services]` parses today and
`kiln install` reports it as unmanaged; the trait will be written when there is a
service to run. Its intended shape:

```rust
trait ServiceProvider {          // Phase 8, not yet written
    fn id(&self) -> &'static str;
    fn available(&self) -> bool;                    // is Docker/Podman present?
    fn up(&self, spec: &ServiceSpec) -> Result<Handle>;
    fn down(&self, handle: &Handle) -> Result<()>;
    fn status(&self, handle: &Handle) -> Result<Status>;
}
```

Docker must never become a hard dependency of the runtime manager.

## The lockfile

```
kiln.toml   what a human asked for      node = "22"
    ↓
kiln.lock   what that resolved to       22.14.0, sha256:…, macos-aarch64
```

The lockfile is *authoritative only while the manifest still agrees with it*.
Each entry records the requirement it was locked against, so editing
`kiln.toml` re-resolves rather than letting a stale lockfile outrank the file a
human just changed. That check is what makes the reuse path safe to take
without asking.

Recording merges rather than replaces: a Linux contributor's `kiln install` must
not delete the macOS entries a colleague committed.

### Locking for a platform you are not on

`kiln lock --all-platforms` works because a provider is handed the platform it
is resolving *for*, never the host it is running on. The artifact URL and digest
for `linux-x86_64-gnu` are as computable from a Mac as from Linux, so one person
can fill in a lockfile the whole team installs from.

It is also nearly free. Node's `SHASUMS256.txt` covers every platform for a
version, and python-build-standalone's `SHA256SUMS` covers every platform for a
release — so the metadata cache serves the second platform and every one after
it. Locking six platforms costs about what locking one does.

A runtime that does not exist for a platform is reported and skipped rather than
fatal. "This project cannot run on Alpine" is a fact about the project, and
`kiln lock` is not the command that should refuse to proceed because of it.

### Drift, and why `--locked` is offline

`--locked` has to fail *before* resolution, or it is not doing its job: the
point is to refuse to install something other than what was reviewed, not to
discover afterwards that it did.

That is possible because the check is structural. For a given platform: does
every requirement have a lockfile entry that was locked against that same
requirement string, and does the lockfile mention nothing the manifest dropped?
If so, resolution will short-circuit entirely to the lockfile and produce
exactly the bytes already on disk — so no network call is needed to know that
nothing would change.

`record` is idempotent down to `generated_by`, which matters more than it
sounds: if upgrading Kiln rewrote that field, `--locked` would start failing
across every project for no real reason and everyone would learn to pass
`--no-verify` to whatever complained.

The format is keyed **by platform**, not written for one machine:

```toml
version = 1
generated_by = "kiln 0.1.0"

[project]
name = "example-app"

[platform.macos-aarch64.runtime.node]
provider = "node"
requirement = "22"
version = "22.14.0"

[platform.macos-aarch64.runtime.node.artifact]
url = "https://nodejs.org/dist/v22.14.0/node-v22.14.0-darwin-arm64.tar.xz"
digest = "sha256:…"
format = "tar.xz"
```

A team is not all on the same hardware. A single-platform lockfile would either
be useless to the person on Linux or be regenerated on every push, which defeats
the point of locking.

The `url` is advisory. The `digest` is what makes an artifact acceptable, so a
mirror is fine and a different payload is not.

A lockfile whose `version` exceeds this build's is detected before the body is
parsed, so an older Kiln says "upgrade" instead of complaining about a field that
did not exist yet.

## The environment

`Environment` composes `PATH` and variables in memory, applies them to one child
process, and is forgotten when that process exits. Kiln never edits a shell
profile or a system directory. `kiln shell` is a child shell and nothing more,
which is why "does Kiln mess with my shell?" has a one-word answer.

### Why Kiln resolves programs itself

`Command::new("node").env("PATH", …)` does **not** reliably use that `PATH` to
find `node`; the interaction is platform-specific, and on some targets lookup
happens against the *parent's* `PATH`. That would mean `kiln run node` silently
executing the system Node.js while claiming to run the project's — the one
guarantee the tool exists to make, resting on unspecified behaviour.

So `kiln-exec` searches the composed `PATH` itself and hands the child an
absolute path. The better error message is a side effect: Kiln can list what the
project *does* provide instead of reporting "No such file or directory".

### Reading the environment without installing it

`kiln run`, `kiln shell`, `kiln list` and `kiln doctor` all need the same
answer — what does the manifest want, what did the lockfile resolve it to, and
is it in the store? That lives once, in `kiln_resolver::activate`. Three copies
of it would be three chances to disagree about what "installed" means, and a
`list` that says "installed" while `run` says "not found" is worse than either.

None of them install as a side effect. Downloading fifty megabytes because
someone asked to run a one-line script is exactly the kind of surprise a
reproducible environment is supposed to remove.

### Signals and exit codes

While a child runs, Kiln stops treating `SIGINT` as fatal — the terminal sends
it to the whole foreground process group, so the child gets its own copy and
decides what to do. If Kiln died on the same signal, the shell prompt would come
back while the child was still shutting down and its exit status would be lost.
The handler is installed through `signal-hook`'s safe API, because every crate
here forbids `unsafe`.

The child's exit code becomes Kiln's, and a child killed by a signal reports
`128 + signal`, exactly as a shell would. `kiln run npm test` has to be usable
wherever `npm test` was.

Three rules, each with a reason:

- **Kiln's directories go first**, in the order they were added, so a project's
  pinned runtime shadows the system one.
- **Duplicates collapse, earliest wins.** A directory already on the inherited
  `PATH` is promoted rather than duplicated, or it would still resolve to the
  system copy.
- **The inherited environment is preserved, not cleared.** Clearing it breaks
  `HOME`, `TERM`, `SSH_AUTH_SOCK` and every credential helper, in exchange for an
  isolation guarantee Kiln does not claim to make. Kiln shadows what a project
  pins; it does not take the machine away.

## Downloads

Two things about the download path were learned by running it against real
hosts rather than by reasoning about it, and both are recorded here because the
reasoning alone gave the wrong answer.

**Downloads get their own timeout profile.** A metadata request should give up
quickly; a download should not. More subtly, artifact URLs redirect — GitHub
sends release downloads to a CDN — and `timeout_recv_response` is measured
across the whole redirect chain rather than reset per hop. A 30-second header
deadline sized for a small JSON fetch therefore fails real downloads
intermittently, which is exactly the kind of bug that looks like a flaky network.
The download agent keeps resolve, connect and *stalled-body* timeouts, and no
header or total deadline.

**Transport failures are retried; verification failures are not.** A dropped
connection part-way through fifty megabytes is a normal event on a real network,
not something to hand back to a person who will only retype the same command.
Three attempts, doubling backoff. A digest mismatch is never retried, because
trying again is the wrong response to bytes that were not what they claimed.

## Concurrency and interruption

Design commitments; Phase 7 hardens and tests them under load.

- Store entries are immutable once written, so readers never see a partial one.
- Writes are `rename(2)` into place from a sibling staging directory.
- `kiln.toml` and `kiln.lock` are written the same way, so an interrupted write
  cannot truncate either.
- No daemon, no lock held across commands. Two concurrent `kiln install` runs
  at worst duplicate a download; the loser of the rename discards its copy and
  uses the winner's, which is safe because both are the same bytes by
  construction.
- Staging directories are removed when their guard drops, so an interrupted
  install leaves nothing behind but at worst one orphan that the next run clears.
- Installs are sequential. Parallel downloads are Phase 7: doing it now would
  mean building multi-bar progress and cross-thread error aggregation before the
  single-threaded path had ever run against a real artifact.

## Security posture

Kiln downloads and executes binaries, so:

- Every artifact is verified against its digest **before** it is unpacked or
  stored, and hashed once, in flight, on the way to disk.
- Extraction refuses absolute paths, `..` components and escaping symlinks, and
  masks away setuid bits. Where `tar` skips an unsafe entry silently, Kiln
  stops — see `SECURITY.md`.
- Nothing in `kiln.toml` is ever passed to a shell. Commands are split into an
  argv at parse time, and shell operators are a parse error.
- `PATH`, `LD_PRELOAD` and `DYLD_INSERT_LIBRARIES` cannot be set by a project.
- Detection reads untrusted repository files with a size cap and never fails the
  command because of what it finds.
- Generated TOML is escaped by hand, so a hostile `package.json` name cannot
  close a quote and inject a key.
- No `unsafe` anywhere: every crate is `#![forbid(unsafe_code)]`.
- Kiln never needs `sudo` and never writes outside its own store and the
  project's own `kiln.toml` / `kiln.lock`.

## Dependencies

Kept small deliberately. Direct dependencies: `serde`, `toml`, `serde_json`,
`sha2`, `clap`, `directories`, `tracing`, `tracing-subscriber`, `ureq`,
`flate2`, `tar`, `indicatif`. TLS accounts for most of the transitive tree.

Notable deliberate absences:

- **No async runtime.** `ureq` is blocking, and nothing in Kiln needs an
  executor. See "Why `kiln-net` exists" above.
- **No `xz` decoder.** Node and CPython both publish `tar.xz` at roughly half
  the size, but decoding it needs either a C library or a much slower pure-Rust
  one. Kiln reads `tar.gz`, which both also publish, and pays a few seconds of
  download to stay dependency-free and portable.
- **No colour crate.** Six ANSI codes did not justify one.
- **No `which` crate.** Program resolution is twenty lines and needs to follow
  Kiln's own rules about empty `PATH` entries and executable bits.
- **No retry crate.** Three attempts with a doubling backoff is a `for` loop.

## Testing

- **Unit tests** live beside the code, covering version parsing and matching,
  digests, platform keys, path composition, manifest parsing and every rejection
  path, project detection, store layout, and lockfile round-trips.
- **Integration tests** (`crates/kiln-cli/tests/cli.rs`) drive the real binary
  against real files and assert on exit codes and rendered output.
- **Fixtures** (`tests/fixtures/`) hold one mistake per invalid manifest, so a
  test expecting a particular diagnostic cannot pass for the wrong reason.

- **Network tests** (`crates/kiln-cli/tests/network.rs`) really download from
  nodejs.org and python-build-standalone, install, and execute the resulting
  binary. They are `#[ignore]`d.

The default suite is hermetic by construction, not by convention: the
integration sandbox passes `--offline`, so a test that reaches for the network
fails instead of quietly downloading. The network tests exist because the things
that can only be wrong against the real world — an index whose shape changed, an
artifact filename that no longer matches — are exactly what a mocked test would
cheerfully confirm are fine.
