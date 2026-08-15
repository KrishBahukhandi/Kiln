# Design Decisions

Each entry states the decision, the alternatives that were considered and
rejected, and what the decision costs. Several of these are the analytical
substance of the system; a paper that reproduced only the feature list and
omitted these would be describing the wrong thing.

---

## 1. Entries are named by the digest of the source archive, not of the unpacked tree

**Decision.** A store entry lives at `store/sha256/<first two hex>/<full hex>/`,
where the digest is of the **archive that was downloaded**, not a hash of the
directory tree it unpacked into.

**Alternative rejected: hash the unpacked tree.** This is what a naive
content-addressed store would do, and it is what Nix effectively does. It
requires committing to a canonical directory-hashing scheme: a traversal order,
a decision about which metadata is part of the identity, and a commitment that
outlives every entry ever written, because changing the scheme invalidates the
entire store.

**Why the archive digest wins.** The archive's digest is *already published and
attested to by the vendor*. Node.js publishes `SHASUMS256.txt`; Go embeds
`sha256` in its release index; Deno ships a `.sha256sum` sidecar. Verifying the
archive means verifying the thing upstream actually signs for. Verifying a tree
hash would mean verifying something only Kiln has ever computed, which is a
weaker claim dressed as a stronger one.

It also means the store needed no canonical tree-hashing scheme in order to
exist, which allowed the decision in §3 to be deferred until it could be made
carefully.

**The cost, and it is a real one.** The entry's name can no longer answer "are
the bytes on disk still the bytes we unpacked?" The archive is deleted after
extraction, so the name is unrecomputable from the tree. Integrity verification
therefore requires a *separate* record — §3.

---

## 2. Everything Kiln records sits beside the runtime, never inside it

**Decision.** `meta.toml`, `tree.manifest` and `last-used` are siblings of
`content/`, not files within it.

**Why.** A runtime that gains files it did not ship is a runtime that is no
longer the thing the publisher distributed. Anything scanning the installation —
a language server enumerating a standard library, a packaging tool, or the
runtime's own module resolution — would see Kiln's bookkeeping as content.

It also gives a free integrity property: an entry directory that exists but has
no `content/` subdirectory is a failed installation, and is distinguishable from
a complete one without reading anything.

---

## 3. Verification compares against a manifest recorded at install time

**Decision.** Immediately after extracting an archive whose published digest has
just been verified, Kiln walks the resulting tree and writes `tree.manifest`.
`kiln cache verify` walks the tree again and compares.

The manifest records, per path: file size, the sha256 of file contents, whether
any execute bit is set, and a symlink's target unfollowed. Directories are
recorded so that losing an empty one is noticed.

**Why this moment.** It is the only moment at which the answer is knowable. The
tree is a deterministic function of an archive that has just been proven
authentic. A manifest taken at any later time could only attest that the tree
matches itself.

**What is deliberately excluded, and why that matters more than what is
included.**

- **Modification times.** Do not survive `cp -a`, a restored backup, or a
  different `tar` implementation. They do not affect whether a runtime works.
- **Ownership.** Same reasoning, plus it changes when a store is copied between
  users.
- **Full permission bits.** A function of the extracting process's umask, so the
  same archive legitimately unpacks to different modes on two machines.

Recording any of these would make verification fail for reasons no user could
act on. A check that cries wolf is a check that gets ignored, and a check that
gets ignored is worse than no check because it produces false confidence.

**What is included and would be easy to omit.** The executable bit. Losing it
breaks a runtime completely while being entirely invisible to a content hash —
the bytes are perfect and the binary will not start. It is the one permission
whose loss is both plausible and fatal.

**Three-valued result, deliberately.** Verification returns *intact*, *damaged*,
or **unverifiable** — the last for entries installed before manifests existed.
Collapsing unverifiable into intact would make an uncheckable entry report as
healthy, which is the single thing a verification command must never do.

**Honest scope.** This detects corruption, not tampering. The manifest lives in
the same directory as the tree it describes, so anything able to rewrite one can
rewrite the other. Detecting deliberate substitution requires an attestation
from outside the store, which is the same missing capability as publisher
signature verification.

---

## 4. Garbage collection evicts by last use, not by reachability

**Decision.** `kiln cache clean` removes entries not used within a window
(default 30 days). Use is recorded by Kiln itself in a `last-used` file.

**The question that cannot be answered.** The question garbage collection
*wants* to answer is "which entries does some project still need?" Kiln cannot
answer it. It keeps no registry of projects — deliberately — so a lockfile on a
disconnected drive, a checkout under a colleague's home directory, or a
repository not yet cloned are all invisible.

**Alternative rejected: maintain a project registry.** This would make
reachability computable. It would also make a stateless tool stateful, introduce
a file that can disagree with reality, and create a new failure mode where
deleting a project directory silently orphans an entry that the registry still
claims is live. The cure is worse than the disease.

**Why not `atime`.** The filesystem already tracks access times, which looks
like exactly the right signal. It is not usable: `relatime` is the Linux default
and `noatime` is common, so `atime` can be hours stale or frozen entirely.
Garbage collection that deleted a runtime someone uses daily would be worse than
no garbage collection at all.

**Asymmetry of error, which is the justification.** Being wrong is cheap in
exactly one direction. Evicting something still wanted costs a re-download.
Keeping something unwanted costs disk. So the default window is conservative,
the command reports before it acts, and deleting requires `--force`.

**What counts as use.** `run`, `shell` and `install`. Deliberately *not* `list`
or `cache list` — otherwise a `kiln list` in a shell prompt would make the store
permanently uncollectable. Writes are bounded to one per entry per hour, so
tracking use costs one small write per hour however often a runtime is invoked.

---

## 5. Concurrency in downloads is implemented, measured, and off by default

**Decision.** `kiln install --jobs N` fetches N runtimes at once. The default is
1.

**Why the default is 1.** Measurement, reported in `04-evaluation.md`.
Concurrency was consistently slower on a bandwidth-limited link: 60 seconds
sequential against 75–110 seconds concurrent for the same three runtimes.

**Why it was kept rather than reverted.** The picture inverts when the
bottleneck is the far end rather than the near one. A CI runner on a very fast
link is limited by the distribution mirror, not by itself, and several streams
do finish sooner there. That is a real situation — it is simply not the
situation most people run `kiln install` in, so it is opt-in rather than
assumed.

**The general point.** Parallel downloading is close to universal in package
managers and is treated as self-evidently beneficial. It is not. When total
bytes are fixed and the link is already saturated, splitting the transfer adds
contention and simultaneous decompression and hashing on one disk, while adding
no bandwidth to divide.

---

## 6. Invariants preserved under concurrency

**Decision.** Two properties hold at any `--jobs` value:

1. **The reported error does not depend on which thread lost.** Every job runs
   to completion and results are sorted back into manifest order before the
   first failure is returned.
2. **A single runtime never spawns a thread.**

**Why the first matters.** A tool whose error message depends on network timing
is a tool whose failures cannot be reproduced or reported. Determinism of
diagnosis was judged worth more than the seconds saved by cancelling siblings
early.

---

## 7. The download stall timeout is enforced by Kiln, not by the HTTP client

**Decision.** Each transfer runs on a worker thread while the calling thread
watches a byte counter. If no bytes arrive for 60 seconds, the transfer is
abandoned and reported.

**Why not the library's timeout.** `ureq`'s `timeout_recv_body` is a deadline
for receiving the *whole* body, not a per-stall timeout. Any value tight enough
to catch a dead connection also kills a healthy large download on a slow link.
Separately, it was observed not to apply at all over TLS. Full account in
`05-engineering-findings.md`.

**The semantic that was actually wanted** — "no bytes at all for a while" — is
the one shape that distinguishes a dead transfer from a slow one, and it
preserves the stated intent that a slow connection should be slow rather than
fail.

**The cost, stated plainly.** A stalled worker is abandoned rather than joined,
because it is blocked in a kernel read that nothing in the process can
interrupt. This is bounded — at most three per download, all ending when the
process does — and each attempt writes to its own uniquely named part file so an
abandoned worker waking later cannot corrupt a retry's download. Only a
digest-verified part file is renamed to the destination.

---

## 8. Version syntax is not Cargo's

**Decision.** `22.14.0` means exactly 22.14.0. Caret and tilde semantics exist
but must be written explicitly.

**Why.** Cargo's implicit caret is right for library dependencies, where
automatic compatible upgrades are the point. It is wrong for a file whose
purpose is to pin a development environment. A developer writing a full version
into `kiln.toml` means that version, and a tool that silently installed a newer
one would be violating the single promise the file exists to make.

**The cost.** Users arriving from Cargo or npm carry the opposite expectation,
so this must be documented prominently, and the error messages for version
requirements state the accepted forms explicitly.

---

## 9. Validation happens inside `Deserialize`

**Decision.** Configuration values are checked within their `Deserialize`
implementations rather than in a validation pass over a parsed structure.

**Why.** The TOML parser supplies the byte span of the offending token at
deserialisation time. That span is what allows a caret to be drawn under the
exact character that is wrong. A check moved to a later pass has a value but no
location, and can only say "the version is invalid" rather than pointing at it.

**The cost.** Validation logic is distributed across type definitions rather
than centralised, which is less tidy to read, and requires a small encoding
convention to carry structured error data through
`serde::de::Error::custom`, which erases types.

This is a case where diagnostic quality was allowed to dictate architecture.

---

## 10. Program resolution is performed explicitly

**Decision.** `kiln run` searches the composed `PATH` itself to locate the
program, rather than setting the environment variable and delegating to the
operating system.

**Why.** `Command::env("PATH", ...)` does not reliably affect how the program
name is looked up; the lookup may use the *parent's* `PATH`. Delegating would
mean `kiln run node` sometimes executing the system's Node rather than the
project's — the exact failure the tool exists to prevent, made silent.

---

## 11. The zip reader was written rather than adopted

**Decision.** Zip support is implemented in `kiln-cache` (roughly 300 lines plus
tests) rather than by adding a zip crate.

**Why.** The compression is DEFLATE, which `flate2` already provides for
`tar.gz`. What a zip crate would add is the container format. Writing it keeps
the path-safety policy *identical* to the tar path — the same three functions
refusing `..`, absolute paths and escaping symlinks — rather than approximately
identical, which is where a discrepancy would hide.

Encryption, multi-disk archives and Zip64 are refused by name rather than
misread. Silently truncating a large entry to its low 32 bits would be the worst
available outcome.

**Honest counterweight.** This is the decision in this document most open to
challenge. A well-maintained zip crate is more battle-tested than 300 fresh
lines, and "we wrote our own parser" is a claim that invites scrutiny. The
argument rests on the narrow supported subset and on the safety-policy
consistency, not on general superiority.

---

## 12. `[services]` is parsed and reported, but nothing is managed

**Decision.** A `[services]` table naming Postgres or Redis is validated and
displayed, and explicitly does nothing.

**Why not omit it.** Rejecting the key would force projects to remove
information they legitimately want recorded. Parsing it keeps the manifest
honest about the project's needs while Kiln is honest about its own limits.

**Why not implement it.** Doing so means container lifecycle, port allocation
and health checking — a phase of work, not a feature. Shipping a partial version
would be worse than shipping none.

---

## 13. Unimplemented functionality fails loudly and names its phase

**Decision.** During development, anything not built returned a distinct error
kind (exit code 9) naming the phase it belonged to. It never silently succeeded.

**Why.** A stub that quietly does nothing is discovered at the worst possible
moment by the person least equipped to diagnose it. An explicit failure is
information.

The corresponding test inverted as the system completed: it originally asserted
that certain commands *did* report themselves unimplemented, and now asserts
that **none** do, which is what would catch a future stub shipping with a phase
marker instead of an implementation.

---

## 14. There is no telemetry, and none is planned

**Decision.** No usage reporting, no crash reporting, no network call not
required to fetch a runtime the user asked for.

**Why it belongs in a design document.** It is a constraint with consequences,
not a marketing position. It means no aggregate data on which runtimes are used,
which errors are common, or which platforms matter — so every judgement in this
document had to be made from reasoning and local measurement rather than from
fleet data. That is a genuine methodological limitation and belongs in a
threats-to-validity discussion.
