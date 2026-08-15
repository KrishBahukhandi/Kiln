# Kiln — Source Set for a Research Paper

## What this collection is

Six documents describing **Kiln**, a local-first reproducible development
environment manager written in Rust. They are written to be used as grounding
sources: every factual claim in them is drawn from the implementation, from the
test suite, or from a measurement recorded during development. Where a number
appears, the conditions under which it was obtained are stated alongside it.

These documents are **not** promotional material. Where Kiln is worse than an
alternative, or where a design choice failed, that is stated in the same voice
as everything else. A paper built from these sources should be able to make
negative claims about the system as easily as positive ones.

## The documents

| File | Contains | Maps to a paper section |
| --- | --- | --- |
| `01-problem-and-context.md` | The problem, why existing tools do not solve it, the requirements that follow | Introduction, Motivation, Related Work |
| `02-architecture.md` | System structure, data flow, on-disk formats, the resolution algorithm | System Design, Implementation |
| `03-design-decisions.md` | Fourteen decisions, each with the alternatives rejected and why | Design Rationale — the analytical core |
| `04-evaluation.md` | Measurements, including one significant negative result | Evaluation, Results |
| `05-engineering-findings.md` | Defects found during development and the method that found them | Lessons Learned, Methodology, Threats to Validity |
| `06-limitations.md` | What Kiln does not do, and the security properties it does not provide | Limitations, Future Work |

## What is genuinely novel here, and what is not

An honest paper should be clear about this, so it is stated up front.

**Not novel.** Content-addressed storage is decades old and is the basis of Nix,
Git, and most modern package managers. Lockfiles are standard practice.
Downloading a tarball, checking a hash, and putting a directory on `PATH` is not
a research contribution.

**Potentially novel, or at least under-documented.** Four things:

1. **Naming a store entry by its source archive's digest rather than by a hash
   of the unpacked tree**, and the consequences that follow — chiefly that
   integrity verification then requires a separately recorded manifest, because
   the entry's own name can no longer answer whether its contents are intact.
   Section 3 of `03-design-decisions.md` develops this.

2. **Garbage collection by last use in a deliberately stateless tool.** The
   reachability question ("which projects still need this?") is unanswerable
   without a registry, and adding a registry would make a stateless tool
   stateful. The paper case is that answering a *different, weaker* question
   honestly beats answering the intended question with invented state.

3. **A measured negative result on download concurrency** (`04-evaluation.md`).
   Parallel downloading is near-universal in package managers and is assumed
   beneficial. On a bandwidth-limited link it was consistently *slower* —
   60s sequential against 75–110s concurrent for the same three artifacts. This
   is a small, reproducible result that contradicts a widely held default.

4. **A concrete failure of library timeout configuration**
   (`05-engineering-findings.md`). A widely used Rust HTTP client's
   `timeout_recv_body` is a whole-body deadline rather than the stall timeout its
   name suggests, and was observed not to apply at all over TLS. The general
   point — that a timeout you configured is not a timeout you have — generalises
   beyond this library.

## Suggested framing

Kiln is best framed not as "a new package manager" but as **a study in what a
reproducibility tool can honestly promise**. The recurring theme across every
section is the same: when the system cannot know something, it says so rather
than guessing. That shows up in garbage collection, in verification, in error
messages, in platform support, and in the roadmap.

A weaker paper would claim Kiln makes environments reproducible. A stronger one
would examine *which parts* of reproducibility are achievable without a
supervisor, a daemon, or a container — and which are not.

## Provenance and honesty constraints

- All performance figures were measured on a single machine (Apple Silicon
  macOS, domestic broadband) and are labelled as such. They are **not**
  benchmarks against other tools; no comparative benchmark was run.
- The system is version 0.1.0. It supports four runtimes and two operating
  systems. Any claim about generality should be scoped accordingly.
- The test counts (568 offline, 23 network) are from the suite at the commit
  these documents describe.
