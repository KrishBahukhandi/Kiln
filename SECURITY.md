# Security

Kiln downloads binaries and puts them on your `PATH`. Integrity is not a feature
here, it is the premise.

## Reporting a vulnerability

Please report security issues privately, through GitHub's **Report a
vulnerability** button on the Security tab, rather than in a public issue.

Include what you found, how to reproduce it, and what an attacker gains. You can
expect an acknowledgement within a few days and an assessment within two weeks.
Kiln is a volunteer project and there is no bounty, but you will be credited in
the advisory unless you would rather not be.

## Threat model

Kiln assumes three things can be hostile:

**The repository.** Cloning a repository and running `kiln install` must not
execute code the repository chose. So:

- Nothing in `kiln.toml` is ever passed to a shell. `[commands]` are split into
  an argv when the manifest is parsed, and shell operators are a parse error
  rather than something that gets interpreted later.
- Commands only run when a developer names one: `kiln run dev`.
- `[environment]` cannot set `PATH`, `LD_PRELOAD` or `DYLD_INSERT_LIBRARIES`.
  The first would disable the environment; the others inject a library into
  every process started in it.
- Project files read during detection (`package.json`, `pyproject.toml`) are
  size-capped and never cause a command to fail based on their contents.
- Generated TOML is escaped by hand, so a name lifted from a hostile
  `package.json` cannot close a quote and add a key of its own.

**The network.** An artifact is acceptable because of its digest, not because of
where it came from.

- Every artifact is hashed as it is downloaded and compared with its expected
  digest **before** it is placed in the store.
- A mismatch is rejected; a rejected artifact never becomes an installation.
- Downloads land in `staging/` and are promoted with `rename(2)` inside the same
  filesystem, so a partial artifact is never reachable under a name that says it
  is complete.
- The `url` in `kiln.lock` is advisory. Mirrors are fine; different bytes are
  not.

**The local filesystem.** Store paths are a pure function of a digest, and a
digest only parses from lowercase hexadecimal, so no path component derived from
one can contain a separator or `..`. Traversal is structurally impossible rather
than filtered out.

**Archives.** Extraction refuses any entry whose path is absolute or contains
`..`, and any symlink whose target escapes the tree. Where `tar` would skip such
an entry and carry on, Kiln stops: a digest-verified artifact from nodejs.org
does not contain one, so finding one means something is wrong that installing
the rest will not fix. Permissions are taken from the archive with setuid,
setgid and sticky bits masked away; the executable bit is kept, because a
runtime without it is not a runtime.

**Downloads are capped** at 1 GiB, so a broken or hostile server cannot stream
until the disk fills.

## What Kiln does not do

- It never requires `sudo`.
- It never modifies system directories, shell profiles, or anything outside
  `~/.kiln` and the project's own `kiln.toml` and `kiln.lock`.
- It runs no background daemon.
- It collects no telemetry, and none is planned.
- It contains no `unsafe` code: every crate is `#![forbid(unsafe_code)]`.

## Current limitations

Stated plainly, because a security document that overstates its coverage is
worse than none.

- **No signature verification.** Kiln verifies digests, not publisher
  signatures. A digest is learned from the publisher over TLS the first time a
  version is resolved (`SHASUMS256.txt` from nodejs.org, `SHA256SUMS` from
  python-build-standalone), and pinned in `kiln.lock` thereafter. That makes it
  trust-on-first-use: as good as that one TLS connection and whoever ran
  `kiln install` first. Anyone reviewing the lockfile diff can check the digests
  against the published ones. Signature checking is planned for Phase 7.
- **Extraction trusts a verified archive.** Kiln does not cap the unpacked size
  of an artifact whose digest already matched what the publisher declared.
  Guarding against a decompression bomb inside an artifact you have already
  agreed to execute would not buy anything.
- **No mirror support.** There is deliberately no way to redirect Kiln at a
  different host, because doing so would also redirect where it learns digests
  from. Phase 7 will add it properly, with the checksum source pinned separately.
- **`kiln cache verify` detects corruption, not tampering.** It compares each
  stored runtime against `tree.manifest`, written when the archive was unpacked
  and its digest had just checked out. That manifest lives in the same directory
  as the tree it describes, so anything with write access to one has write
  access to the other: it catches bit-rot, truncation and accidental edits, and
  it would not catch a deliberate substitution that updated both. Making that
  detectable needs an attestation from outside the store, which is the same
  missing piece as signature verification above.
- **No sandboxing.** A runtime Kiln installs runs with your full user
  permissions, exactly as it would if you installed it yourself. Kiln reproduces
  environments; it does not contain them.
- **`kiln shell` is not isolation.** It shadows what a project pins and leaves
  the rest of your environment intact, deliberately. Treat it as a `PATH`
  change, not a boundary.

## Supported versions

Kiln is pre-1.0. Only the latest release receives fixes.
