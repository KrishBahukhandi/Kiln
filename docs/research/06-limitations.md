# Limitations, Security Properties, and Future Work

Written so that a paper can state what Kiln does **not** do as precisely as what
it does. A reproducibility tool that overstates its guarantees is worse than one
with modest guarantees clearly described, because the overstatement is what
people act on.

---

## 1. Security properties

### What is guaranteed

- **Every artifact is verified against a cryptographic digest before it is
  unpacked or executed.** Bytes are hashed as they arrive; a mismatch destroys
  the file rather than warning.
- **The digest, not the URL, is what makes an artifact acceptable.** A mirror is
  fine; a different payload from the official host is not.
- **Digests are recorded in `kiln.lock` and are reviewable in a diff.** A change
  to what will be installed is a visible change to a committed file.
- **Extraction cannot escape its destination.** Absolute paths, `..` components
  and escaping symlinks are refused, not skipped.
- **setuid, setgid and sticky bits are masked away** during extraction.
- **Nothing runs with elevated privileges.** Kiln writes to its own directory,
  makes no system modification, and installs no daemon.
- **No `unsafe` code.** Every crate is `#![forbid(unsafe_code)]`.

### What is not guaranteed

**No publisher signature verification.** This is the most significant gap. Kiln
verifies digests, not signatures. A digest is learned from the publisher over
TLS the first time a version is resolved and pinned in `kiln.lock` thereafter.
That is **trust-on-first-use**: the guarantee is only as good as that one TLS
connection and whoever ran `kiln install` first. An attacker controlling the
distribution host at the moment of first resolution controls both the artifact
and the digest recorded for it.

Mitigations are partial: lockfile digests are reviewable against published
values, and once pinned they cannot change silently.

**Verification detects corruption, not tampering.** `kiln cache verify` compares
a store entry against a manifest written at install time. That manifest lives in
the same directory as the tree it describes, so anything with write access to
one has write access to the other. It catches bit-rot, truncation, partial
restores and accidental edits. It does not catch a deliberate substitution that
updated both. Closing this needs the same missing capability as signatures: an
attestation from outside the store.

**Extraction trusts a verified archive's size.** The unpacked size is not
capped. Guarding against a decompression bomb inside an artifact whose digest
already matched what the publisher declared, and which the user is about to
execute, would not buy anything.

**No mirror support, deliberately.** There is no way to redirect Kiln at a
different host, because doing so would also redirect where it learns digests
from — converting a trust-on-first-use weakness into a trust-anyone weakness.
Adding mirrors properly requires pinning the checksum source separately.

**No sandboxing.** An installed runtime executes with the user's full
permissions, exactly as it would if installed by hand. Kiln reproduces
environments; it does not contain them.

**`kiln shell` is not isolation.** It shadows what a project pins and leaves the
rest of the environment intact, deliberately. It is a `PATH` composition, not a
boundary.

---

## 2. Functional limitations

**Four runtimes.** Node.js, Python, Go, Deno. Any claim about generality should
be scoped to these.

**Two operating systems.** macOS and Linux. `Os::Windows` exists in the platform
enumeration so that lockfiles remain portable across a mixed team, but no
provider builds for it, and neither `.zip`-based layout conventions nor Windows
`PATH` semantics are implemented.

**Services are declared but unmanaged.** A `[services]` table naming Postgres or
Redis is parsed, validated and displayed, and nothing starts. A project needing
a database still needs another mechanism.

**No language-level package management.** npm, pip and their equivalents remain
responsible for libraries. Kiln pins the runtime beneath them. A fully
reproducible environment therefore requires both Kiln's lockfile *and* the
language ecosystem's, and Kiln makes no attempt to reconcile the two.

**`tar.xz` is recognised but not unpacked.** Those downloads are roughly half
the size, but decompressing xz requires either a C library or a substantially
slower pure-Rust implementation, and every runtime that ships xz ships gzip
beside it. The format remains in the enumeration so a lockfile mentioning it
still parses.

**Zip support covers a deliberate subset.** Encryption, multi-disk archives and
Zip64 are refused by name rather than misread.

---

## 3. Blocked work, and what blocks it

Two runtimes were investigated and deliberately not implemented. Both are
instructive because the blocker is a design question rather than effort.

**Bun.** Ships zip, which is now supported. Its only complete version index is
the GitHub releases API, which permits 60 unauthenticated requests per hour per
IP address — shared by everyone behind one NAT, which describes most corporate
networks and CI fleets. Deno avoids this entirely by publishing
`dl.deno.land/versions.json`. Adding Bun requires deciding what Kiln does when
that budget is exhausted: authenticate, degrade, cache more aggressively, or
fail clearly.

**Rust.** Publishes a combined `tar.gz`, so extraction is not the obstacle. The
archive unpacks into *per-component* directories — `cargo/bin`, `rustc/bin`,
`rust-std-<triple>/`, `clippy-preview/bin` — and `rustc` locates its standard
library relative to its own path. Unpacked as-is it would install cleanly and
then fail to compile anything, which is the worst failure shape available: an
apparently successful installation that is broken at first real use. Supporting
it requires either merging components into a single prefix, or executing the
vendor's `install.sh` — and running a vendor's shell script is a materially
different trust model from anything else Kiln does.

Both were left undone rather than approximated. This is consistent with the
project's rule that a stub which silently succeeds is worse than an honest
failure.

---

## 4. Methodological limitations

These bear directly on how much weight the evaluation can carry.

- **No comparative benchmark.** Kiln has not been measured against nvm, asdf,
  mise, Nix or containers on any axis.
- **Single-machine, single-network measurements**, taken partly during
  demonstrable network degradation.
- **No user study.** Diagnostics are argued for on design grounds. No user has
  been observed reading a Kiln error message, and the claim that four-part
  errors reduce time-to-resolution is untested.
- **No fleet data, by construction.** The absence of telemetry means every
  judgement about which errors are common, which runtimes matter, or which
  platforms are used was made from reasoning rather than evidence. This is a
  deliberate trade with a real epistemic cost.
- **No multi-machine reproducibility experiment.** The central claim is enforced
  structurally by digest equality and is tested for a CI runner installing from
  a committed lockfile, but has not been demonstrated across a heterogeneous
  fleet over time.
- **The garbage collection window (30 days) is a judgement, not a measured
  optimum.**
- **Version 0.1.0.** Nothing here has been exposed to sustained real-world use,
  which is the condition under which most reproducibility tools acquire their
  interesting failure modes.

---

## 5. Future work, in the order the project would take it

1. **Publisher signature verification.** The largest genuine gap. Requires
   deciding where signatures are learned from and how trust is anchored — a
   design question, not an implementation task.
2. **Mirrors, with the checksum source pinned independently of the artifact
   source.** Naturally follows from (1) and is unsafe before it.
3. **More runtimes**, once Bun's rate-limit question and Rust's layout question
   are answered.
4. **Services.** Container lifecycle, port allocation, health checks. A phase of
   work.
5. **Windows.** `.zip` layout conventions, `PATH` semantics, and the absence of
   an executable bit — which interacts directly with what the tree manifest
   records.
6. **Adaptive download concurrency.** The measured result suggests the correct
   parallelism depends on where the bottleneck sits. Measuring it at runtime
   rather than asking the user to guess is a plausible improvement, and would
   turn the negative result of `04-evaluation.md` into a positive one.

---

## 6. The honest summary

Kiln makes one narrow promise well: **given a lockfile, the runtimes installed
are byte-identical to those recorded, on any supported platform, verifiably, and
without touching the rest of the machine.**

It does not make the environment reproducible. It makes the *toolchain*
reproducible, which is the part that was previously undocumented, and leaves
language packages, services, operating system libraries and developer
configuration exactly where it found them.

Whether that narrow promise is worth a tool is the question a paper should
actually argue about.
