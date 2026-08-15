# Evaluation and Measurements

## Measurement conditions, stated first

Every figure below was obtained on **one machine**: Apple Silicon macOS
(Darwin 25.5.0), 8-crate release build (`cargo build --release`), over domestic
broadband whose throughput varied considerably during the measurement period —
a fact which is itself one of the findings.

**No comparative benchmark against another tool was run.** No claim is made here
that Kiln is faster or slower than nvm, asdf, mise, Nix or a container workflow.
Any such claim in a paper would need an experiment that has not been performed.

Single-machine, single-network measurements support claims about *relative*
behaviour under identical conditions (sequential versus concurrent, cached
versus uncached). They do not support claims about absolute performance.

---

## 1. Cold installation, one runtime at a time

Fresh store, one runtime per project, timed end to end including release-index
resolution, download, digest verification, extraction, manifest recording and
store insertion.

| Runtime | Version | Archive | Unpacked | Files | Run A | Run B |
| --- | --- | --- | --- | --- | --- | --- |
| Node.js | 22.14.0 | 44.9 MB | 165.1 MB | 4,808 | 16 s | 14 s |
| Python | 3.13.15 | — | 62.4 MB | 1,646 | 31 s | 27 s |
| Go | 1.26.6 | — | 218.2 MB | 15,034 | 20 s | 19 s |
| Deno | 2.9.5 | 36.7 MB | 77.2 MB | 1 | ~19 s | — |

Deno is the outlier worth noting: a single 77 MB executable rather than a tree.
Go is the opposite extreme at 15,034 files.

## 2. Warm installation

With the artifacts already in the store, a second `kiln install` of a
two-runtime project completes in **approximately 2 milliseconds** and performs
no network access. The store lookup is a directory-existence check on a path
derived purely from the digest.

This is the property that makes the content-addressed store worth its
complexity: two projects needing byte-identical runtimes converge on one
directory without either knowing the other exists, and the second project pays
nothing.

## 3. The negative result: concurrency is slower

The measurement that changed a design decision.

**Setup.** One project pinning Node 22.14.0, Python 3.13, Go 1.26 — 130.8 MB of
artifacts. Empty store each time. Sequential runs measured as three separate
single-runtime installs into separate stores and summed; concurrent runs
measured as one install with `--jobs 3`.

| Mode | Elapsed |
| --- | --- |
| Sequential (14 + 27 + 19) | **60 s** |
| Sequential (16 + 31 + 20) | **67 s** |
| Concurrent, 3 at once | 75 s |
| Concurrent, 3 at once | 93 s |
| Concurrent, 3 at once | 110 s |

**Concurrency lost every run.** Median sequential ≈ 63 s; median concurrent
≈ 93 s. Aggregate throughput fell from roughly 2.0 MB/s to roughly 1.4 MB/s.

**Interpretation.** Total bytes are fixed. When a single download already
saturates the link, splitting the transfer three ways adds TCP contention and
three simultaneous streams of decompression and hashing competing for one disk,
while adding no bandwidth to divide among them. Variance also rose sharply —
the concurrent spread was 75–110 s against a sequential spread of 60–67 s —
which is consistent with contention rather than with measurement noise alone.

**Threats to this result.** Single machine, single network, small sample
(three concurrent runs, two sequential). The link was demonstrably unstable
during the period (see §6). A fast, high-latency link — the CI case — would
plausibly invert the result, which is precisely why the capability was retained
behind `--jobs` rather than removed.

**Why it is still worth reporting.** Parallel downloading is a near-universal
default in package managers and is generally treated as self-evidently
beneficial. This is a small, reproducible counterexample under a common
condition: a bandwidth-limited consumer connection.

## 4. Verification cost

`kiln cache verify` reads every byte of every stored file and compares against
the recorded manifest.

| Store contents | Files | Bytes read | Elapsed |
| --- | --- | --- | --- |
| Node 22.14.0 | 4,808 | 165.1 MB | **0.43 s** |
| Node + Python + Go | 21,488 | 445.7 MB | a few seconds |

The design concern was that full re-hashing would be too expensive to be usable,
which would push toward a weaker check — hashing only sizes and paths. The
measurement removed that concern: 165 MB in under half a second on an SSD is not
a cost worth trading integrity for.

**Detection sensitivity.** Verified by flipping a single bit in one file inside
the 4,808-file Node tree — specifically byte 10 of
`lib/node_modules/npm/package.json`, XOR 0x01, preserving file length:

```
✗ node 22.14.0  1 path differs
    lib/node_modules/npm/package.json  same size, different contents
```

Exit code 6. Restoring the original byte returned the store to `1 runtime
intact`. The same-length mutation is the case a size-and-path check would
report as healthy.

## 5. Test suite

| Suite | Count | Runtime | Network |
| --- | --- | --- | --- |
| Offline | 568 | ~8 s | none |
| Network | 23 | 388 s | real upstream feeds |

The offline suite is structurally hermetic: every test receives its own
`KILN_HOME`, and the integration sandbox passes `--offline` so a test cannot
reach an upstream host by accident. This property was added after discovering
that three early tests were silently downloading roughly 50 MB.

The network suite is `#[ignore]`d by default and exercises the real
distribution feeds — the only thing that can catch an upstream changing its
release-index format. A hand-written fixture keeps passing indefinitely after
the real feed has moved on.

Static analysis: `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo doc` with `RUSTDOCFLAGS=-D warnings` both clean. No `unsafe` code.

## 6. An unplanned observation: network degradation

During one measurement period the link to a Cloudflare-fronted CDN degraded to
approximately **3.8 KB/s** while remaining established. A download of a
38.5 MB artifact reached 14.6 MB after 20 minutes 35 seconds and was still
progressing.

This is reported for two reasons. First, it is the condition under which the
concurrency measurement was taken, and is a genuine threat to that result's
validity. Second, it exposed a latent defect — the transfer had no effective
timeout and would have continued indefinitely — which is documented in
`05-engineering-findings.md`. A slow network is not usually thought of as a
test, but it functioned as one.

## 7. Dependency footprint

37 direct dependencies resolving to 145 total. No async runtime: HTTP is
blocking, which keeps the provider trait object-safe and avoids pulling in an
executor. Approximately 19,550 lines of Rust across eight crates.

Offered as context for the "small dependency tree" claim rather than as a
result. No comparison against other tools' footprints was measured.

## 8. Self-hosting as a functional test

Kiln's own documentation website pins its Node.js version in a `kiln.toml` and
is built by Kiln:

```
kiln install      # installs the pinned Node
kiln run build    # tsc -b && vite build
```

Output: 219.47 kB JavaScript (67.98 kB gzipped), 20.47 kB CSS (4.92 kB
gzipped), built in 106 ms.

The continuous integration job that performs this deliberately does **not** use
`actions/setup-node`. The claim under test is that cloning a repository and
running Kiln is sufficient; providing Node by another route would make the job
pass even if that claim stopped being true.

## 9. What has not been evaluated

Stated so that a paper does not overclaim:

- **No comparison against other tools.** Not on speed, disk usage, or
  correctness.
- **No user study.** Claims about diagnostic quality are design arguments, not
  measured outcomes. Nobody has been observed reading a Kiln error message.
- **No multi-machine reproducibility experiment.** The central claim — that a
  lockfile produces identical environments across machines — is enforced
  structurally by digest verification and is tested for a CI runner installing
  from a committed lockfile, but has not been measured across a heterogeneous
  fleet.
- **No long-run cache behaviour.** The garbage collection policy has not been
  evaluated against real usage over months; the 30-day default is a judgement,
  not a measured optimum.
- **Linux is tested in CI but every measurement here is macOS.**
- **Windows is unimplemented**, so no data exists.
