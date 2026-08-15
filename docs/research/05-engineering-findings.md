# Engineering Findings and Methodology

A record of defects found during development, how each was found, and what
generalises. The common thread is a single methodological commitment: **verify
against reality rather than against a model of reality.** Every defect below was
found by running the real thing — a real feed, a real download, a real built
artifact, a live process — and several were structurally invisible to the test
suite, to code review, and to the type system.

---

## 1. A timeout that was neither what it was named nor applied

**The most substantial finding, and the one that generalises furthest.**

**Symptom.** A `kiln install` sat for 20 minutes 35 seconds receiving roughly
3.8 KB/s from a CDN, having written 14.6 MB of a 38.5 MB artifact, with a
progress bar that had not moved. The configured limit was 120 seconds. It would
have continued indefinitely.

**Investigation.** Three steps, each narrowing the claim:

1. The process was alive, not deadlocked. `lsof` showed an established TLS
   socket and a partial file that was *still growing* — 13.4 MB, then 14.6 MB
   five minutes later. So this was a crawl, not a hang.
2. `sample(1)` on the live process gave the stack:
   `rustls::read_tls` → `TransportAdapter::read` → `maybe_await_input` →
   `TcpTransport::await_input` → `recv` → `__recvfrom`, blocked in the kernel.
3. A local server was written that sends headers, 4 KB of body, and then goes
   silent forever without closing — reproducing the failure shape exactly. With
   the identical client configuration over **plain HTTP** the timeout fired
   correctly: `ERRORED after 3.001749083s: timeout: receive body`.

**Two distinct defects, both masked by a comment asserting the opposite.**

*First:* `ureq`'s `timeout_recv_body` is documented as "Max duration for
receiving the response body" — a deadline for the **whole body**, not the
per-stall timeout its name suggests. As configured at 120 seconds it did not
mean "give up after two minutes of silence"; it meant "fail any download taking
longer than two minutes", which on a slow link is every large runtime. Users
would have encountered that spurious failure before ever encountering the hang.

*Second:* over TLS it did not apply at all. `TransportAdapter` is constructed
with its timeout set to `Duration::NotHappening`, and `maybe_update_timeout`
calls `set_read_timeout` only when a finite duration is computed. A
`NotHappening` timeout therefore leaves the socket in blocking mode, and a
stalled HTTPS connection blocks in `recvfrom` with no deadline of any kind.

**Fix.** The semantic actually wanted — "no bytes at all for a while" — is the
only shape that distinguishes a dead transfer from a slow one, and no
per-stall option was available. Since a blocking read cannot be timed out by the
thread performing it, the transfer moved onto a worker thread while the calling
thread watches an atomic byte counter, failing after 60 seconds without
progress. A slow-but-alive transfer may now take as long as it likes, which is
what the module had always claimed.

The abandoned worker is a genuine cost: it remains blocked in a kernel read
nothing can interrupt. It is bounded at three per download, and each attempt
writes to its own part file so a worker waking later cannot corrupt a retry.

**What generalises.** *A timeout you configured is not a timeout you have.*
Timeout semantics vary between libraries and between transports within one
library, names routinely mislead, and configuration is silently ignored rather
than rejected. The only reliable check is to make the failure happen and watch.
For a security-relevant path, enforcing the property in your own code — where
the semantics are yours — may be worth the complexity.

**Detection difficulty.** Invisible to the type system, to review, to the
offline suite, and to the network suite under good conditions. It required a
degraded network, and it presented as an environmental problem rather than a
bug.

---

## 2. An untagged unit variant that broke every install

**Symptom.** Node.js installation failed entirely.

**Cause.** The Node.js release index carries an `lts` field that is either
`false` or a string naming the release line. It was modelled as a serde
`untagged` enum with a unit variant for the boolean case. An untagged unit
variant matches only JSON `null` — so `"lts": false` failed to deserialise, and
because the failure occurred while parsing the array, it failed the *entire*
index rather than one entry.

**Fix.** A variant holding the boolean.

**What generalises.** Real feeds contain the awkward cases; hand-written
fixtures contain what the author imagined. This is the specific reason the
contribution guide requires provider tests to use a slice of the *real* index
including its irregular entries.

---

## 3. Extraction that skipped unsafe entries silently

**Cause.** The `tar` crate's bulk `unpack` *skips* an entry that would escape
the destination and continues. That is safe, and silent.

**Why silence is wrong here.** A digest-verified artifact from an official host
containing a path-traversal entry means something is badly wrong. Installing the
remaining nine thousand files will not fix it, and continuing produces an
installation that looks complete.

**Fix.** Kiln iterates entries itself and fails loudly on absolute paths, `..`
components, and symlinks resolving outside the tree.

**What generalises.** "Safe" and "correct" are different properties. A library
that protects you while hiding the anomaly has made a reasonable default choice
for its median caller and the wrong one for a security-critical path.

---

## 4. A redirect chain measured as one deadline

**Symptom.** Python downloads failed roughly two times in three.

**Cause.** `timeout_recv_response` was set to 30 seconds and is measured across
an entire redirect chain rather than reset per hop. GitHub release downloads
redirect to a CDN, so a deadline sized for a small JSON fetch was being applied
to a multi-hop artifact download.

**Fix.** Downloads were given their own client configuration with no header
deadline, plus bounded retry. Verified 5 of 5 successful afterwards.

**Relationship to finding 1.** Same class, opposite direction: first a timeout
too tight for its actual scope, later a timeout that did not apply at all. Both
came from assuming a configured value meant what its name implied.

---

## 5. Two commands that disagreed about the same lockfile

**Symptom.** `kiln lock --all-platforms` and `kiln lock --check --all-platforms`
disagreed. The writer skipped Linux musl (Node.js publishes no musl build); the
checker demanded it. The CI gate was therefore impossible to pass.

**Fix.** Both routed through the same planning function.

**What generalises.** Two code paths answering the same question will
eventually disagree. This recurred later — installation and activation each
composed `PATH` independently, and were consolidated into one `bin_paths()`
function *before* they had a chance to diverge, prompted by this experience.

---

## 6. An idempotence bug that would have surfaced only on upgrade

**Cause.** Recording a lockfile bumped its `generated_by` field
unconditionally, including when nothing else changed. Any Kiln upgrade would
therefore have made `--locked` fail in CI for every project, with a diff
containing only a version string.

**Found by** reasoning about the field's lifetime, not by a test.

**What generalises.** Time-varying and version-varying metadata inside a file
that a `--locked` check compares is a trap that only springs on upgrade — after
release, on someone else's CI.

---

## 7. Dead code that had been written and tested

**Cause.** `clean_staging` was implemented, unit-tested, and never called by any
command. The tests passed because they invoked it directly.

**What generalises.** Unit tests confirm a function works; they say nothing
about whether it *runs*. Coverage of a function is not coverage of a path.

---

## 8. A suggestion algorithm broken by a two-character name

**Symptom.** Adding the Go provider caused `Registry::suggest` to propose `go`
for the inputs `io`, `ai`, and the empty string.

**Cause.** A fixed edit-distance budget of two. For a two-character name, every
two-character string is within budget.

**Fix.** Names of three characters or fewer match on shared prefix only.

**What generalises.** Edit-distance thresholds must scale with the length of
what they compare. The defect was introduced by *data* — a new short name — not
by changed code, so no diff of the suggestion logic would have revealed it.

---

## 9. A fixture that would rot on a fixed date

**Cause.** A test planted a store entry with `installed_unix = 1780000000`. As
the calendar advanced past that timestamp, freshly planted entries would appear
abandoned and garbage-collection tests would begin failing.

**Fix.** The fixture records the current time.

**What generalises.** A hardcoded absolute timestamp in a test that reasons
about elapsed time is a scheduled failure.

---

## 10. A build-time construct hoisted out of a media query

**Symptom.** The documentation website was permanently dark. The entire light
palette was dead code.

**Cause.** Tailwind v4's `@theme` is a build-time construct that is hoisted to a
single `:root`. A second `@theme` inside `@media (prefers-color-scheme: dark)`
therefore does not become conditional — it overwrites the first,
unconditionally. The generated CSS contained zero `prefers-color-scheme` blocks.

**Found by** inspecting the built output. Reading the source would never have
revealed it, because the source expressed the intent correctly.

**What generalises.** When a tool transforms source into output, the output is
the artifact under test. This is the same principle as testing providers against
the real release index rather than a fixture.

---

## 11. Content clipped without a scrollbar

**Cause.** CSS grid items default to `min-width: auto`, so a monospace table
sized its track wider than a 390 px viewport. The page did not scroll — it
clipped, silently.

**What generalises.** A failure that produces *no* signal is worse than one that
produces a wrong signal. A horizontal scrollbar would have been a visible
defect; silent truncation was invisible until measured programmatically.

---

## 12. Duplicate work masquerading as a hang

**Not a defect in Kiln, but a methodological trap worth recording.**

A test run that had exceeded a tool timeout was detached and continued executing
unnoticed. Twenty-two minutes later it was still running, competing for
bandwidth with a subsequent run of the same tests and making the second run
appear stalled. The first diagnosis — "the suite is hung" — was wrong; the
correct one was "two suites are fighting for a 3.8 KB/s link".

**What generalises.** When measuring, verify that only the thing being measured
is running. A process list would have shown this immediately; it was not
checked until the evidence stopped fitting.

---

## Methodological summary

The defects above cluster into four detection classes:

| Class | Examples | What finds them |
| --- | --- | --- |
| Only visible against real upstream data | 2, 4 | Real-feed tests on a schedule |
| Only visible in generated output | 10, 11 | Inspect the artifact, not the source |
| Only visible under adverse conditions | 1 | Degraded networks; fault injection |
| Only visible by reasoning about lifetime | 6, 7, 9 | Review focused on time and reachability |

None of the four is addressed by unit testing, type checking, or code review of
a diff. Each required a distinct deliberate practice. That is the finding most
worth generalising from this project: the defects that survive a good type
system and a good test suite are systematically of a *different kind* than the
ones those tools catch, and they need their own instruments.
